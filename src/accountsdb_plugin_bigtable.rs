/// Main entry for the Bigtable plugin
use {
    crate::{
        accounts_selector::AccountsSelector, bigtable_client::AsyncBigtableClient,
        transaction_selector::TransactionSelector,
    },
    bs58,
    log::*,
    serde_derive::{Deserialize, Serialize},
    serde_json,
    solana_accountsdb_plugin_interface::accountsdb_plugin_interface::{
        AccountsDbPlugin, AccountsDbPluginError, ReplicaAccountInfoVersions,
        ReplicaBlockInfoVersions, ReplicaTransactionInfoVersions, Result, SlotStatus,
    },
    solana_measure::measure::Measure,
    solana_metrics::*,
    std::{fs::File, io::Read, time::Duration},
    thiserror::Error,
};

#[derive(Default)]
pub struct AccountsDbPluginBigtable {
    client: Option<AsyncBigtableClient>,
    accounts_selector: Option<AccountsSelector>,
    transaction_selector: Option<TransactionSelector>,
}

impl std::fmt::Debug for AccountsDbPluginBigtable {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Ok(())
    }
}

/// The Configuration for the Bigtable plugin
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccountsDbPluginBigtableConfig {
    /// The path of the Bigtable credential file
    pub credential_path: Option<String>,

    /// Bigtable timeout
    pub timeout: Option<Duration>,

    /// Controls the number of threads establishing connections to
    /// the Bigtable server. The default is 10.
    pub threads: Option<usize>,

    /// Controls the batch size when bulk loading accounts.
    /// The default is 10.
    pub batch_size: Option<usize>,

    /// Controls whether to panic the validator in case of errors
    /// writing to Bigtable server. The default is false
    pub panic_on_db_errors: Option<bool>,

    /// Indicates whether to store historical data for accounts
    pub store_account_historical_data: Option<bool>,

    /// Controls whether to index the token owners. The default is false
    pub index_token_owner: Option<bool>,

    /// Controls whetherf to index the token mints. The default is false
    pub index_token_mint: Option<bool>,
}

#[derive(Error, Debug)]
pub enum AccountsDbPluginBigtableError {
    #[error("Error connecting to the backend data store. Error message: ({msg})")]
    DataStoreConnectionError { msg: String },

    #[error("Error preparing data store schema. Error message: ({msg})")]
    DataSchemaError { msg: String },

    #[error("Error preparing data store schema. Error message: ({msg})")]
    ConfigurationError { msg: String },
}

impl AccountsDbPlugin for AccountsDbPluginBigtable {
    fn name(&self) -> &'static str {
        "AccountsDbPluginBigtable"
    }

    /// Do initialization for the Bigtable plugin.
    ///
    /// # Format of the config file:
    /// * The `accounts_selector` section allows the user to controls accounts selections.
    /// "accounts_selector" : {
    ///     "accounts" : \["pubkey-1", "pubkey-2", ..., "pubkey-n"\],
    /// }
    /// or:
    /// "accounts_selector" = {
    ///     "owners" : \["pubkey-1", "pubkey-2", ..., "pubkey-m"\]
    /// }
    /// Accounts either satisyfing the accounts condition or owners condition will be selected.
    /// When only owners is specified,
    /// all accounts belonging to the owners will be streamed.
    /// The accounts field supports wildcard to select all accounts:
    /// "accounts_selector" : {
    ///     "accounts" : \["*"\],
    /// }
    /// "store_account_historical_data", optional, set it to 'true', to store historical account data to account_audit
    /// table.
    /// * "threads" optional, specifies the number of worker threads for the plugin. A thread
    /// maintains a Bigtable connection to the server. The default is '10'.
    /// * "batch_size" optional, specifies the batch size of bulk insert when the AccountsDb is created
    /// from restoring a snapshot. The default is '10'.
    /// * "panic_on_db_errors", optional, contols if to panic when there are errors replicating data to the
    /// Bigtable database. The default is 'false'.
    /// * "transaction_selector", optional, controls if and what transaction to store. If this field is missing
    /// None of the transction is stored.
    /// "transaction_selector" : {
    ///     "mentions" : \["pubkey-1", "pubkey-2", ..., "pubkey-n"\],
    /// }
    /// The `mentions` field support wildcard to select all transaction or all 'vote' transactions:
    /// For example, to select all transactions:
    /// "transaction_selector" : {
    ///     "mentions" : \["*"\],
    /// }
    /// To select all vote transactions:
    /// "transaction_selector" : {
    ///     "mentions" : \["all_votes"\],
    /// }
    /// # Examples
    ///
    /// {
    ///    "libpath": "/home/solana/target/release/libsolana_accountsdb_plugin_postgres.so",
    ///    "host": "host_foo",
    ///    "user": "solana",
    ///    "threads": 10,
    ///    "accounts_selector" : {
    ///       "owners" : ["9oT9R5ZyRovSVnt37QvVoBttGpNqR3J7unkb567NP8k3"]
    ///    }
    /// }

    fn on_load(&mut self, config_file: &str) -> Result<()> {
        solana_logger::setup_with_default("info");
        info!(
            "Loading plugin {:?} from config_file {:?}",
            self.name(),
            config_file
        );
        let mut file = File::open(config_file)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;

        let result: serde_json::Value = serde_json::from_str(&contents).unwrap();
        self.accounts_selector = Some(Self::create_accounts_selector_from_config(&result));
        self.transaction_selector = Some(Self::create_transaction_selector_from_config(&result));

        let result: serde_json::Result<AccountsDbPluginBigtableConfig> =
            serde_json::from_str(&contents);
        match result {
            Err(err) => {
                return Err(AccountsDbPluginError::ConfigFileReadError {
                    msg: format!(
                        "The config file is not in the JSON format expected: {:?}",
                        err
                    ),
                })
            }
            Ok(config) => {
                let client = AsyncBigtableClient::new(&config)?;
                self.client = Some(client);
            }
        }

        Ok(())
    }

    fn on_unload(&mut self) {
        info!("Unloading plugin: {:?}", self.name());

        match &mut self.client {
            None => {}
            Some(client) => {
                client.join();
            }
        }
    }

    fn update_account(
        &mut self,
        account: ReplicaAccountInfoVersions,
        slot: u64,
        is_startup: bool,
    ) -> Result<()> {
        let mut measure_all = Measure::start("accountsdb-plugin-bigtable-update-account-main");
        match account {
            ReplicaAccountInfoVersions::V0_0_1(account) => {
                let mut measure_select =
                    Measure::start("accountsdb-plugin-bigtable-update-account-select");
                if let Some(accounts_selector) = &self.accounts_selector {
                    if !accounts_selector.is_account_selected(account.pubkey, account.owner) {
                        return Ok(());
                    }
                } else {
                    return Ok(());
                }
                measure_select.stop();
                inc_new_counter_debug!(
                    "accountsdb-plugin-bigtable-update-account-select-us",
                    measure_select.as_us() as usize,
                    100000,
                    100000
                );

                debug!(
                    "Updating account {:?} with owner {:?} at slot {:?} using account selector {:?}",
                    bs58::encode(account.pubkey).into_string(),
                    bs58::encode(account.owner).into_string(),
                    slot,
                    self.accounts_selector.as_ref().unwrap()
                );

                match &mut self.client {
                    None => {
                        return Err(AccountsDbPluginError::Custom(Box::new(
                            AccountsDbPluginBigtableError::DataStoreConnectionError {
                                msg: "There is no connection to the Bigtable database.".to_string(),
                            },
                        )));
                    }
                    Some(client) => {
                        let mut measure_update =
                            Measure::start("accountsdb-plugin-bigtable-update-account-client");
                        let result = { client.update_account(account, slot, is_startup) };
                        measure_update.stop();

                        inc_new_counter_debug!(
                            "accountsdb-plugin-bigtable-update-account-client-us",
                            measure_update.as_us() as usize,
                            100000,
                            100000
                        );

                        if let Err(err) = result {
                            return Err(AccountsDbPluginError::AccountsUpdateError {
                                msg: format!("Failed to persist the update of account to the Bigtable database. Error: {:?}", err)
                            });
                        }
                    }
                }
            }
        }

        measure_all.stop();

        inc_new_counter_debug!(
            "accountsdb-plugin-bigtable-update-account-main-us",
            measure_all.as_us() as usize,
            100000,
            100000
        );

        Ok(())
    }

    fn update_slot_status(
        &mut self,
        slot: u64,
        parent: Option<u64>,
        status: SlotStatus,
    ) -> Result<()> {
        info!("Updating slot {:?} at with status {:?}", slot, status);

        match &mut self.client {
            None => {
                return Err(AccountsDbPluginError::Custom(Box::new(
                    AccountsDbPluginBigtableError::DataStoreConnectionError {
                        msg: "There is no connection to the Bigtable database.".to_string(),
                    },
                )));
            }
            Some(client) => {
                let result = client.update_slot_status(slot, parent, status);

                if let Err(err) = result {
                    return Err(AccountsDbPluginError::SlotStatusUpdateError{
                        msg: format!("Failed to persist the update of slot to the Bigtable database. Error: {:?}", err)
                    });
                }
            }
        }

        Ok(())
    }

    fn notify_end_of_startup(&mut self) -> Result<()> {
        info!("Notifying the end of startup for accounts notifications");
        match &mut self.client {
            None => {
                return Err(AccountsDbPluginError::Custom(Box::new(
                    AccountsDbPluginBigtableError::DataStoreConnectionError {
                        msg: "There is no connection to the Bigtable database.".to_string(),
                    },
                )));
            }
            Some(client) => {
                let result = client.notify_end_of_startup();

                if let Err(err) = result {
                    return Err(AccountsDbPluginError::SlotStatusUpdateError{
                        msg: format!("Failed to notify the end of startup for accounts notifications. Error: {:?}", err)
                    });
                }
            }
        }
        Ok(())
    }

    fn notify_transaction(
        &mut self,
        transaction_info: ReplicaTransactionInfoVersions,
        slot: u64,
    ) -> Result<()> {
        match &mut self.client {
            None => {
                return Err(AccountsDbPluginError::Custom(Box::new(
                    AccountsDbPluginBigtableError::DataStoreConnectionError {
                        msg: "There is no connection to the Bigtable database.".to_string(),
                    },
                )));
            }
            Some(client) => match transaction_info {
                ReplicaTransactionInfoVersions::V0_0_1(transaction_info) => {
                    if let Some(transaction_selector) = &self.transaction_selector {
                        if !transaction_selector.is_transaction_selected(
                            transaction_info.is_vote,
                            transaction_info.transaction.message().account_keys_iter(),
                        ) {
                            return Ok(());
                        }
                    } else {
                        return Ok(());
                    }
                    let result = client.log_transaction_info(transaction_info, slot);

                    if let Err(err) = result {
                        return Err(AccountsDbPluginError::SlotStatusUpdateError{
                                msg: format!("Failed to persist the transaction info to the Bigtable database. Error: {:?}", err)
                            });
                    }
                }
            },
        }

        Ok(())
    }

    fn notify_block_metadata(&mut self, block_info: ReplicaBlockInfoVersions) -> Result<()> {
        match &mut self.client {
            None => {
                return Err(AccountsDbPluginError::Custom(Box::new(
                    AccountsDbPluginBigtableError::DataStoreConnectionError {
                        msg: "There is no connection to the Bigtable database.".to_string(),
                    },
                )));
            }
            Some(client) => match block_info {
                ReplicaBlockInfoVersions::V0_0_1(block_info) => {
                    let result = client.update_block_metadata(block_info);

                    if let Err(err) = result {
                        return Err(AccountsDbPluginError::SlotStatusUpdateError{
                                msg: format!("Failed to persist the update of block metadata to the Bigtable database. Error: {:?}", err)
                            });
                    }
                }
            },
        }

        Ok(())
    }

    /// Check if the plugin is interested in account data
    /// Default is true -- if the plugin is not interested in
    /// account data, please return false.
    fn account_data_notifications_enabled(&self) -> bool {
        self.accounts_selector
            .as_ref()
            .map_or_else(|| false, |selector| selector.is_enabled())
    }

    /// Check if the plugin is interested in transaction data
    fn transaction_notifications_enabled(&self) -> bool {
        self.transaction_selector
            .as_ref()
            .map_or_else(|| false, |selector| selector.is_enabled())
    }
}

impl AccountsDbPluginBigtable {
    fn create_accounts_selector_from_config(config: &serde_json::Value) -> AccountsSelector {
        let accounts_selector = &config["accounts_selector"];

        if accounts_selector.is_null() {
            AccountsSelector::default()
        } else {
            let accounts = &accounts_selector["accounts"];
            let accounts: Vec<String> = if accounts.is_array() {
                accounts
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|val| val.as_str().unwrap().to_string())
                    .collect()
            } else {
                Vec::default()
            };
            let owners = &accounts_selector["owners"];
            let owners: Vec<String> = if owners.is_array() {
                owners
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|val| val.as_str().unwrap().to_string())
                    .collect()
            } else {
                Vec::default()
            };
            AccountsSelector::new(&accounts, &owners)
        }
    }

    fn create_transaction_selector_from_config(config: &serde_json::Value) -> TransactionSelector {
        let transaction_selector = &config["transaction_selector"];

        if transaction_selector.is_null() {
            TransactionSelector::default()
        } else {
            let accounts = &transaction_selector["mentions"];
            let accounts: Vec<String> = if accounts.is_array() {
                accounts
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|val| val.as_str().unwrap().to_string())
                    .collect()
            } else {
                Vec::default()
            };
            TransactionSelector::new(&accounts)
        }
    }

    pub fn new() -> Self {
        Self::default()
    }
}

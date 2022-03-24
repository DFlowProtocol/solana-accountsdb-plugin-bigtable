# solana-geyser-plugin-bigtable
Solana AccountsDb Plugin for Google Bigtable
This is a WIP -- do not use for production!

## BigTable Setup

### Development Environment
The Cloud BigTable emulator can be used during development/test.  See
https://cloud.google.com/bigtable/docs/emulator for general setup information.

Process:
1. Make sure install GCP CLI, see https://cloud.google.com/sdk/docs/install-sdk.
2. Install the Bigtable CLI CBT https://cloud.google.com/bigtable/docs/cbt-overview.
3. Run `gcloud beta emulators bigtable start` in the background
4. Run `$(gcloud beta emulators bigtable env-init)` to establish the `BIGTABLE_EMULATOR_HOST` environment variable
5. Run `./scripts/init-bigtable.sh` to configure the emulator
6. Develop/test

### Production Environment
Export a standard `GOOGLE_APPLICATION_CREDENTIALS` environment variable to your
service account credentials.  The project should contain a BigTable instance
called `solana-ledger` that has been initialized by running the `./init-bigtable.sh` script.

Depending on what operation mode is required, either the
`https://www.googleapis.com/auth/bigtable.data` or
`https://www.googleapis.com/auth/bigtable.data.readonly` OAuth scope will be
requested using the provided credentials.

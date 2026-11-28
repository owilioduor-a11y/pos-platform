# POS Platform (SoftPOS + Retail POS)

An offline-first Point-of-Sale system built in **Rust** and **Kotlin (Jetpack Compose)**. Accepts contactless NFC card taps (debit, credit, and Visa/Mastercard crypto cards) and M-Pesa mobile money, designed for micro-merchants and multi-cashier retail stores.

## Repository Structure
- `crates/pos-domain` — Shared models, currency/tax math, and payment orchestration traits.
- `crates/pos-core` — Local SQLite persistence, offline sync engine, and UniFFI bindings for Android.
- `crates/pos-server` — Cloud REST API (`axum`), PostgreSQL ledger, and M-Pesa Daraja / acquirer webhooks.
- `android/` — Android app (Jetpack Compose UI, ML Kit barcode scanner, and SoftPOS SDK adapter).
- `docs/` — Architecture, PCI security, Kenya launch checklist, and global roadmap.

## Prerequisites
- **Rust:** Stable toolchain (`rustup update stable`)
- **Android Cross-Compile:**
  ```bash
  rustup target add aarch64-linux-android armv7-linux-androideabi
  cargo install cargo-ndk
  ```
- **Database:** PostgreSQL 15+ (for `pos-server`) and SQLite 3 (for `pos-core`)
- **Mobile:** Android Studio (SDK 34+, NDK r26+)

## Running Locally
1. **Run all Rust checks & tests:**
   ```bash
   cargo test --workspace
   ```
2. **Start the cloud API (`pos-server`):**
   ```bash
   cp .env.example .env
   cargo run -p pos-server
   ```
3. **Build the Rust Android native library (`pos-core`):**
   ```bash
   cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs build -p pos-core --release
   ```

## Android configuration

The app no longer hardcodes its backend endpoint or credentials. Values are injected into `BuildConfig` at build time:

| Value | Environment variable | Gradle property | Debug default |
| --- | --- | --- | --- |
| Server URL | `POS_SERVER_URL` | `-PposServerUrl` | `http://10.0.2.2:8080` |
| API token (JWT) | `POS_API_TOKEN` | `-PposApiToken` | `dev-token` |
| Merchant ID (UUID) | `POS_MERCHANT_ID` | `-PposMerchantId` | `00000000-0000-7000-8000-000000000001` |

Environment variables take precedence over `-P` properties. Debug builds work out of the box against the Android emulator host; override any value to target a different backend.

Release builds **fail closed**: `validateReleaseConfig` runs before resource processing and rejects missing values, plaintext `http://` URLs, the `dev-token` placeholder, short tokens, and non-UUID merchant IDs. The app repeats those checks at startup, so a misconfigured release APK cannot silently talk to a development backend.

TLS is also enforced by the OS: `src/main/res/xml/network_security_config.xml` blocks cleartext and trusts only system CAs. The debug source set overlays it to allow the emulator's `http://10.0.2.2:8080` backend.

```bash
POS_SERVER_URL=https://pos.example.com \
POS_API_TOKEN='eyJ...' \
POS_MERCHANT_ID='00000000-0000-7000-8000-000000000001' \
  gradle assembleRelease
```

Never commit real tokens to `android/gradle.properties` (it is tracked); prefer environment variables.

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

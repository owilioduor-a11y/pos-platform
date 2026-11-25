# AGENTS.md — SoftPOS & Retail POS Platform

## Project Overview
An offline-first, multi-currency in-person POS system supporting NFC Tap-to-Phone card payments (traditional & crypto-backed Visa/Mastercard/Amex debit and credit cards) and M-Pesa, launching in Kenya and scaling globally. Designed for both micro-merchants (Quick Charge mode) and retail stores (Catalog, Barcode, Inventory, Multi-Cashier).

## Tech Stack
- **Language:** Rust (2021 edition, stable)
- **Workspace Crates:**
  - `crates/pos-domain`: Pure Rust domain models, money/tax math, and payment provider traits (zero platform/IO dependencies).
  - `crates/pos-core`: Offline-first SQLite engine (`sqlx`), sync queue, and `uniffi` bindings for Android.
  - `crates/pos-server`: Cloud API (`axum`), PostgreSQL ledger (`sqlx`), JWT auth, and payment webhooks (M-Pesa Daraja, card acquirers).
- **Mobile Client (`android/`):** Thin Kotlin + Jetpack Compose shell wrapping `pos-core` via UniFFI and integrating certified Android SoftPOS AAR SDKs.

## Non-Negotiable Engineering Rules
1. **Financial Math:** NEVER use `f32` or `f64` for money, prices, taxes, or quantities. Always use `i64` minor units (e.g., cents/pesewa) paired with ISO 4217 currency codes (`KES`, `USD`, `EUR`, `GBP`) and `rust_decimal` for tax rate calculations with explicit rounding (`RoundingStrategy::MidpointAwayFromZero`).
2. **Card & PCI Security:** NEVER store, log, or transmit raw card numbers (PAN), CVVs, or magnetic track data. Only store opaque processor transaction references/tokens, masked last-4 digits, and safe EMV receipt metadata (`CardScheme`, `CardFundingType`, masked AID, auth code). Crypto-backed cards (e.g., Coinbase, Crypto.com, Binance Visa/Mastercard) process over standard EMV contactless rails and require no separate on-chain handling.
3. **Offline Idempotency:** Every sale and payment attempt must carry a client-generated UUIDv7 idempotency key so network retries never create duplicate charges or double-deduct stock.
4. **No Custom NFC Crypto/EMV:** Do not write raw NFC APDU payment logic. All card tap payments must go through a clean `TapToPayKernel` interface that wraps certified third-party SoftPOS SDKs.
5. **Error Handling:** Use `thiserror` in library crates (`pos-domain`, `pos-core`) and `anyhow` + structured HTTP JSON errors in `pos-server`. Avoid `.unwrap()` or `.expect()` in production code paths.

## Mandatory Verification Before Completing Any Task
Run these commands and ensure zero warnings/errors before finishing:
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`

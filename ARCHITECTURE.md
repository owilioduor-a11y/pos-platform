# System Architecture

pos-platform/
├── .github/
│   └── workflows/
│       ├── rust-ci.yml                  # Runs cargo fmt, clippy, and cargo test on every PR
│       └── android-ci.yml               # Cross-compiles pos-core via cargo-ndk & builds Android APK
├── docs/
│   ├── SECURITY_PCI.md                  # PCI MPoC & card/crypto-card compliance rules
│   ├── KENYA_LAUNCH.md                  # eCitizen Ltd registration, Daraja, local acquirers & eTIMS
│   └── ROADMAP.md                       # Phased build checklist (Sandbox -> Kenya -> Global)
├── crates/
│   ├── pos-domain/                      # Pure Rust domain models, money/tax math & payment traits
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs                   # Crate exports & domain error types
│   │       ├── money.rs                 # Checked minor-unit (i64) Money & ISO Currency structs
│   │       ├── tax.rs                   # Inclusive/exclusive VAT engine (Kenya 16% default)
│   │       ├── catalog.rs               # Merchant profiles (Micro/Retail), Product, SKU, Barcode
│   │       ├── sale.rs                  # Cart, CartItem, Sale, SplitPayment & UUIDv7 idempotency
│   │       └── payment.rs               # PaymentGateway, TapToPayKernel traits & EMV card metadata
│   ├── pos-core/                        # Offline-first SQLite engine, sync outbox & UniFFI bridge
│   │   ├── Cargo.toml
│   │   ├── build.rs                     # UniFFI scaffolding build script
│   │   ├── migrations/
│   │   │   └── 0001_offline_init.sql    # Local SQLite schema (catalog cache, sales, sync_outbox)
│   │   └── src/
│   │       ├── lib.rs                   # UniFFI exported interface (create_sale, record_tap, sync)
│   │       ├── db.rs                    # Local SQLite connection pool & migrations
│   │       ├── repository.rs            # Offline CRUD for products, carts & completed sales
│   │       └── sync.rs                  # Background HTTP outbox worker pushing to pos-server
│   └── pos-server/                      # Cloud Axum REST API, PostgreSQL ledger & webhooks
│       ├── Cargo.toml
│       ├── migrations/
│       │   └── 0001_server_init.sql     # PostgreSQL schema (merchants, users, ledger, payments)
│       └── src/
│           ├── main.rs                  # Server entrypoint, tracing & graceful shutdown
│           ├── config.rs                # Typed env loader (.env)
│           ├── error.rs                 # Structured JSON API error responses
│           ├── auth.rs                  # JWT login, password hashing & merchant/role extractors
│           ├── routes/
│           │   ├── mod.rs               # Axum router assembly
│           │   ├── health.rs            # GET /healthz readiness check
│           │   ├── catalog.rs           # GET /api/v1/catalog & inventory updates
│           │   ├── sync.rs              # POST /api/v1/sync/sales (idempotent batch ingestion)
│           │   └── payments.rs          # M-Pesa STK Push trigger & Daraja/acquirer webhooks
│           └── gateways/
│               ├── mod.rs               # Multi-region payment provider router
│               ├── daraja.rs            # Safaricom Daraja M-Pesa Express (STK Push) client
│               └── card_acquirer.rs     # Server-side card settlement & refund adapter
├── android/                             # Thin Kotlin + Jetpack Compose POS mobile shell
│   ├── build.gradle.kts                 # Root Gradle config
│   ├── settings.gradle.kts              # Gradle module settings
│   ├── gradle.properties
│   └── app/
│       ├── build.gradle.kts             # App dependencies (Compose, ML Kit Barcode, JNA for UniFFI)
│       └── src/
│           └── main/
│               ├── AndroidManifest.xml  # NFC, Camera (barcode), Bluetooth (printer) permissions
│               ├── jniLibs/
│               │   └── arm64-v8a/       # Compiled libpos_core.so output (gitignored)
│               └── java/com/posplatform/app/
│                   ├── MainActivity.kt                  # App entrypoint & UniFFI library loader
│                   ├── PosViewModel.kt                  # State bridge between Compose UI & pos-core
│                   ├── softpos/
│                   │   ├── TapToPayManager.kt           # Interface wrapping certified SoftPOS SDK
│                   │   └── MockTapToPayManager.kt       # 2-sec simulated NFC tap for dev/testing
│                   ├── hardware/
│                   │   ├── BarcodeScanner.kt            # Google ML Kit camera barcode scanner
│                   │   └── ThermalPrinter.kt            # ESC/POS Bluetooth receipt printer helper
│                   └── ui/
│                       ├── QuickChargeScreen.kt         # Micro-merchant numeric keypad + Tap/M-Pesa
│                       ├── RetailCatalogScreen.kt       # Retail store product grid, cart & scanner
│                       └── TapPromptSheet.kt            # EMVCo contactless tap animation & status
├── AGENTS.md                            # Global AI agent rules & mandatory verification commands
├── README.md                            # Local setup, prerequisites & build commands
├── ARCHITECTURE.md                      # System diagram, UniFFI boundary & multi-region routing
├── Cargo.toml                           # Rust workspace root (members & shared dependencies)
├── docker-compose.yml                   # Local PostgreSQL 16 instance for pos-server development
├── .env.example                         # Safe environment variable template
└── .gitignore                           # Ignores .env, /target, *.db, and compiled .so/.apk files


## 1. High-Level Design
The system separates **business logic & financial ledgers (Rust)** from **certified hardware/NFC execution (Android Kotlin)** so the core POS engine remains portable, testable, and reusable across countries and payment processors.

```text
+---------------------------------------------------------------+

|                    Android POS App (Kotlin)                   |
|  - Jetpack Compose UI (Quick Charge Mode & Retail Catalog)    |
|  - TapToPayManager (Wraps Certified Acquirer SoftPOS AAR)     |
|  - Hardware Drivers (Bluetooth Thermal Printer, Barcode Scan) |
+-------------------------------+-------------------------------+
                                | UniFFI (Generated Kotlin <-> Rust)
+-------------------------------v-------------------------------+

|                  pos-core (Rust Shared Library)               |
|  - Offline-First SQLite Store (Sales, Carts, Local Catalog)   |
|  - Sync Outbox Queue (UUIDv7 Idempotency Keys)                |
|  - Business Logic (pos-domain: Money, Tax, Split Payments)    |
+-------------------------------+-------------------------------+
                                | HTTPS / JSON (TLS 1.3 + JWT)
+-------------------------------v-------------------------------+

|                  pos-server (Rust Axum Backend)               |
|  - PostgreSQL Double-Entry / Immutable Transaction Ledger     |
|  - Multi-Processor Router (Kenya Acquirers, Stripe, Adyen)    |
|  - M-Pesa Daraja STK Push & Callback Webhook Handler         |
+---------------------------------------------------------------+
```

## 2. Merchant Modes
- **Micro-Merchant Mode:** Opens directly to a fast numeric keypad ("Quick Charge") for instant NFC card tap or M-Pesa STK Push with SMS/WhatsApp digital receipts.
- **Retail Store Mode:** Full product catalog, barcode scanning, stock deduction, cashier PIN switching, shift Z-reports, and split payments (e.g., partial M-Pesa + partial card tap).

## 3. Multi-Region Payment Orchestration
- `TapToPayKernel` (client) and `PaymentGateway` (server) abstract processor differences.
- **Kenya:** Local acquiring partner SoftPOS SDK (KES) + Safaricom Daraja API (M-Pesa).
- **Global:** Region-based routing to Stripe Terminal Tap to Pay, Adyen Tap to Pay, or NMI without altering `pos-domain` or `pos-core`.

# Product & Engineering Roadmap

## Phase 1: Offline Core & Sandbox MVP (Weeks 1–4)
- [x] `pos-domain`: Money math, 16% VAT engine, cart, split payments, and payment traits.
- [x] `pos-server`: Axum API, PostgreSQL ledger, JWT auth, and Safaricom Daraja sandbox STK Push.
- [x] `pos-core`: Offline SQLite store, UUIDv7 sync queue, and UniFFI bindings.
- [x] `android/`: Jetpack Compose UI (Quick Charge + Retail Catalog) with mock 2-second NFC tap.

## Phase 2: Kenya Pilot & Certification (Weeks 5–10)
- [ ] Complete Kenyan Private Limited Company (Ltd) registration and corporate bank account.
- [ ] Sign ISV partner agreement with Kenyan card acquirer and obtain SoftPOS Android AAR SDK.
- [ ] Replace mock `TapToPayManager` with live certified SoftPOS SDK and complete test-card certification.
- [ ] Pilot with 10–20 Nairobi micro-merchants and retail shops.

## Phase 3: Global Multi-Processor Expansion (Months 3+)
- [ ] Add `StripeTerminalTapToPay` and `AdyenTapToPay` adapters for US, UK, EU, and Australia.
- [ ] Add Tap to Pay on iPhone (Swift wrapper sharing `pos-core` via UniFFI).
- [ ] Multi-store analytics dashboard, staff permissions, and automated KRA eTIMS / local tax connectors.

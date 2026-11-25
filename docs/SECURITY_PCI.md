# Card Security & PCI MPoC Guidelines

## 1. Scope Reduction Strategy
This software operates as an **Independent Software Vendor (ISV) POS layer** that delegates all sensitive cardholder data (CHD) capture, PIN-on-glass entry, and EMV cryptographic operations to a **PCI MPoC-certified SoftPOS SDK** provided by a licensed acquiring bank or PSP.

## 2. Traditional & Crypto-Backed Cards
- Consumer crypto cards (Coinbase, Crypto.com, Binance, MetaMask, etc.) run over **Visa, Mastercard, or Amex EMV contactless networks**.
- From the POS perspective, they behave identically to standard debit/prepaid cards: the customer taps, the SoftPOS SDK authorizes the fiat amount, and the acquirer settles fiat to the merchant.
- **Allowed Metadata to Persist:**
  - Processor Transaction ID / Retrieval Reference Number (RRN)
  - Authorization Code (`auth_code`)
  - Masked PAN last 4 digits (e.g., `**** 4242`)
  - Card Scheme (`Visa`, `Mastercard`, `Amex`, `Other`) & Funding Type (`Debit`, `Credit`, `Prepaid`)
  - EMV Application Identifier (`AID`) & Cryptogram result for receipt printing
- **Strictly Forbidden:**
  - Full PAN (13–19 digits), CVV/CVC, PIN blocks, or raw NFC APDU payloads.

## 3. Device Integrity & Transport
- The SoftPOS SDK enforces runtime device attestation (blocking rooted/jailbroken devices, active screen recorders, or debuggers in production).
- All API traffic between `pos-core` and `pos-server` must enforce TLS 1.2+ with certificate pinning in production builds.

# Kenya Launch & Compliance Checklist

## 1. Business Incorporation (Prerequisite for Live APIs)
- Incorporate a **Private Limited Company (Ltd)** via the Business Registration Service (BRS) on **eCitizen** (`brs.go.ke`).
- Obtain Company KRA PIN, Certificate of Incorporation, CR12, and open a corporate KES/USD bank account.
- **Regulatory Posture:** Launch as a POS technology vendor/ISV where merchants onboard directly under a licensed acquirer's PSP/acquiring license, avoiding upfront Central Bank of Kenya (CBK) PSP licensing capital requirements.

## 2. Payment Rail Partners (Kenya)
- **Contactless Card Tap (SoftPOS):** Apply for ISV/partner SDK access with a Kenyan bank or licensed PSP offering Tap-to-Phone (e.g., KCB Bank, Pesapal, DPO Group / Network International, or white-label MPoC kernel providers integrated with a local bank).
- **Mobile Money (M-Pesa):** Register on the **Safaricom Daraja Portal** for M-Pesa Express (STK Push) and C2B Paybill/Till webhooks.

## 3. Tax & KRA eTIMS Readiness
- Store item-level tax codes (`VAT 16%`, `Zero-Rated 0%`, `Exempt`) on every `SaleItem` in `pos-domain`.
- Keep an extensible `TaxComplianceHook` trait in `pos-server` to transmit signed electronic tax invoices to KRA **eTIMS** (via OSCU/VSCU APIs) once in production.

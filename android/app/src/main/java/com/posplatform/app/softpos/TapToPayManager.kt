package com.posplatform.app.softpos

/** Result of a contactless card tap. Contains ONLY masked/opaque fields —
 * never PAN, CVV, or track data (PCI MPoC rules). */
data class TapResult(
    val approved: Boolean,
    val scheme: String,        // "visa" | "mastercard" | "amex" | "other"
    val funding: String,       // "debit" | "credit" | "prepaid"
    val panLast4: String,      // masked last-4 only
    val authCode: String?,
    val processorRef: String,  // opaque acquirer reference
    val declineReason: String?,
)

/**
 * Interface wrapping a certified SoftPOS SDK (PCI MPoC).
 * Implementations delegate all EMV/NFC APDU work to the vendor AAR —
 * this codebase must NEVER implement raw NFC crypto itself.
 */
interface TapToPayManager {
    val kernelName: String

    /** Whether the device supports contactless payments right now. */
    suspend fun isAvailable(): Boolean

    /**
     * Prompt the user to tap a card and charge [amountMinor].
     * Blocks (suspends) until the SDK returns approve/decline.
     */
    suspend fun charge(amountMinor: Long, currency: String): TapResult
}

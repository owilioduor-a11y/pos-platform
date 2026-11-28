package com.posplatform.app.softpos

/**
 * Selects the contactless kernel at build variant level. Only debug builds may
 * use the simulator; a release without a certified acquirer AAR fails closed.
 */
object TapToPayFactory {
    fun create(useMockKernel: Boolean): TapToPayManager =
        if (useMockKernel) MockTapToPayManager() else UnavailableTapToPayManager
}

/** Placeholder that never approves a tap until a certified SoftPOS SDK is wired in. */
object UnavailableTapToPayManager : TapToPayManager {
    override val kernelName: String = "unavailable"

    override suspend fun isAvailable(): Boolean = false

    override suspend fun charge(amountMinor: Long, currency: String): TapResult = TapResult(
        approved = false,
        scheme = "other",
        funding = "other",
        panLast4 = "",
        authCode = null,
        processorRef = "",
        declineReason = "Certified SoftPOS kernel is not configured",
    )
}

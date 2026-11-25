package com.posplatform.app.softpos

import kotlinx.coroutines.delay
import java.util.UUID

/**
 * Development/testing implementation that simulates a ~2 second NFC tap
 * and returns an approved result with synthetic masked metadata.
 * Replace with the certified acquirer SoftPOS SDK wrapper for production.
 */
class MockTapToPayManager : TapToPayManager {

    override val kernelName: String = "mock-softpos"

    override suspend fun isAvailable(): Boolean = true

    override suspend fun charge(amountMinor: Long, currency: String): TapResult {
        // Simulate radio + EMV transaction latency.
        delay(2000)
        return TapResult(
            approved = true,
            scheme = "visa",
            funding = "debit",
            panLast4 = "4242",
            authCode = "A1B2C3",
            processorRef = "mock_${UUID.randomUUID()}",
            declineReason = null,
        )
    }
}

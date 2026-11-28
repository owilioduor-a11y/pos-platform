package com.posplatform.app.softpos

import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class TapToPayFactoryTest {

    @Test
    fun debug_factory_uses_mock_kernel() {
        assertTrue(TapToPayFactory.create(useMockKernel = true) is MockTapToPayManager)
    }

    @Test
    fun release_factory_fails_closed_without_certified_kernel() = runBlocking {
        val manager = TapToPayFactory.create(useMockKernel = false)
        assertFalse(manager.isAvailable())
        val result = manager.charge(amountMinor = 1000, currency = "KES")
        assertFalse(result.approved)
        assertEquals("Certified SoftPOS kernel is not configured", result.declineReason)
    }
}

package com.posplatform.app.ui

import org.junit.Assert.assertEquals
import org.junit.Test

class MoneyFormatTest {

    @Test
    fun parses_decimal_kes_into_minor_units() {
        assertEquals(0L, parseMinorUnits(""))
        assertEquals(12_345L, parseMinorUnits("123.45"))
        assertEquals(1_230L, parseMinorUnits("12.3"))
        assertEquals(100L, parseMinorUnits("1"))
        assertEquals(123_400L, parseMinorUnits("1234"))
    }

    @Test
    fun parses_negative_amounts_without_losing_the_fraction() {
        assertEquals(-150L, parseMinorUnits("-1.50"))
        assertEquals(-12_345L, parseMinorUnits("-123.45"))
    }

    @Test
    fun rejects_invalid_and_overflowing_amounts() {
        assertEquals(0L, parseMinorUnits("not-a-number"))
        assertEquals(0L, parseMinorUnits("1.2.3"))
        assertEquals(0L, parseMinorUnits("92233720368547758.08"))
        assertEquals(Long.MAX_VALUE, parseMinorUnits("92233720368547758.07"))
    }

    @Test
    fun formats_amounts_without_abs_overflow() {
        assertEquals("123.45", formatMinorUnits(12_345L))
        assertEquals("-1.50", formatMinorUnits(-150L))
        assertEquals("92233720368547758.07", formatMinorUnits(Long.MAX_VALUE))
        assertEquals("-92233720368547758.08", formatMinorUnits(Long.MIN_VALUE))
    }
}

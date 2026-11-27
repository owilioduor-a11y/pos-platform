package com.posplatform.app.ui

/** Convert a decimal KES string to integer cents without floating point. */
internal fun parseMinorUnits(input: String): Long {
    val parts = input.split('.', limit = 2)
    val major = parts[0].ifEmpty { "0" }.toLongOrNull() ?: 0L
    val fraction = (parts.getOrNull(1) ?: "").padEnd(2, '0').take(2)
    val minor = fraction.toIntOrNull() ?: 0
    return major * 100 + minor
}

/** Display integer cents as a decimal amount without floating point. */
internal fun formatMinorUnits(amountMinor: Long): String {
    val abs = kotlin.math.abs(amountMinor)
    val sign = if (amountMinor < 0) "-" else ""
    return "$sign${abs / 100}.${(abs % 100).toString().padStart(2, '0')}"
}

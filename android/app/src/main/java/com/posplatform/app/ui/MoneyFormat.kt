package com.posplatform.app.ui

/** Convert a decimal KES string to integer cents without floating point. */
internal fun parseMinorUnits(input: String): Long {
    val trimmed = input.trim()
    if (trimmed.isEmpty()) return 0L
    val negative = trimmed.startsWith('-')
    val unsigned = if (negative || trimmed.startsWith('+')) trimmed.drop(1) else trimmed
    val parts = unsigned.split('.', limit = 2)
    val major = parts[0].ifEmpty { "0" }.toLongOrNull() ?: return 0L
    val fraction = (parts.getOrNull(1) ?: "").padEnd(2, '0').take(2)
    val minor = fraction.toIntOrNull() ?: return 0L
    val magnitude = runCatching {
        Math.addExact(Math.multiplyExact(major, 100L), minor.toLong())
    }.getOrElse { return 0L }
    if (magnitude < 0) return 0L
    return if (negative) -magnitude else magnitude
}

/** Display integer cents as a decimal amount without floating point. */
internal fun formatMinorUnits(amountMinor: Long): String {
    // kotlin.math.abs(Long.MIN_VALUE) overflows back to a negative value.
    if (amountMinor == Long.MIN_VALUE) return "-92233720368547758.08"
    val abs = kotlin.math.abs(amountMinor)
    val sign = if (amountMinor < 0) "-" else ""
    return "$sign${abs / 100}.${(abs % 100).toString().padStart(2, '0')}"
}

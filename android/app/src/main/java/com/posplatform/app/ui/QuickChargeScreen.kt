package com.posplatform.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.posplatform.app.PosViewModel
import com.posplatform.app.TapState

/**
 * Micro-merchant "Quick Charge" screen: fast numeric amount entry
 * plus Tap / M-Pesa / Cash tender options.
 */
@Composable
fun QuickChargeScreen(
    viewModel: PosViewModel,
    contentPadding: PaddingValues,
) {
    var amountText by remember { mutableStateOf("") }
    var phone by remember { mutableStateOf("") }
    val totalMinor by viewModel.cartTotalMinor.collectAsState()
    val tapState by viewModel.tapState.collectAsState()
    val message by viewModel.lastMessage.collectAsState()

    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(contentPadding)
            .padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Text("Quick Charge", style = MaterialTheme.typography.headlineMedium)
        Spacer(Modifier.height(8.dp))
        Text(
            "KSh ${"%.2f".format(totalMinor / 100.0)}",
            fontSize = 44.sp,
            fontWeight = FontWeight.Bold,
            color = MaterialTheme.colorScheme.primary,
        )

        Spacer(Modifier.height(24.dp))

        OutlinedTextField(
            value = amountText,
            onValueChange = { v ->
                amountText = v.filter { it.isDigit() || it == '.' }
                val major = amountText.toDoubleOrNull() ?: 0.0
                viewModel.setAdhocAmount((major * 100).toLong())
            },
            label = { Text("Amount (KES)") },
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal),
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )

        Spacer(Modifier.height(12.dp))

        OutlinedTextField(
            value = phone,
            onValueChange = { phone = it.filter { c -> c.isDigit() || c == '+' } },
            label = { Text("M-Pesa phone (optional)") },
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Phone),
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )

        Spacer(Modifier.height(24.dp))

        // Tender buttons
        Button(
            onClick = { viewModel.chargeByTap() },
            enabled = totalMinor > 0 && tapState == TapState.IDLE,
            modifier = Modifier.fillMaxWidth().height(64.dp),
        ) {
            Text(if (tapState == TapState.WAITING) "Hold card near phone…" else "Tap to Pay")
        }

        Spacer(Modifier.height(12.dp))

        Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            Button(
                onClick = { viewModel.chargeByMpesa(phone) },
                enabled = totalMinor > 0 && phone.length >= 9,
                modifier = Modifier.weight(1f).height(56.dp),
                colors = ButtonDefaults.buttonColors(
                    containerColor = MaterialTheme.colorScheme.tertiary,
                ),
            ) { Text("M-Pesa") }

            Button(
                onClick = { viewModel.chargeCash() },
                enabled = totalMinor > 0,
                modifier = Modifier.weight(1f).height(56.dp),
                colors = ButtonDefaults.buttonColors(
                    containerColor = MaterialTheme.colorScheme.secondary,
                ),
            ) { Text("Cash") }
        }

        Spacer(Modifier.height(24.dp))

        message?.let {
            Card(modifier = Modifier.fillMaxWidth()) {
                Text(it, modifier = Modifier.padding(12.dp))
            }
        }

        Spacer(Modifier.height(16.dp))
        Text(
            viewModel.cartSummary.collectAsState().value,
            style = MaterialTheme.typography.bodySmall,
        )
    }
}

package com.posplatform.app.ui

import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.posplatform.app.PosViewModel
import com.posplatform.app.TapState

/**
 * EMVCo-style contactless tap prompt shown while a card payment is
 * in flight. Pulsing ring animation + status text.
 */
@Composable
fun TapPromptSheet(viewModel: PosViewModel) {
    val tapState by viewModel.tapState.collectAsState()
    if (tapState == TapState.IDLE) return

    val transition = rememberInfiniteTransition(label = "tapPulse")
    val pulse by transition.animateFloat(
        initialValue = 0.6f,
        targetValue = 1f,
        animationSpec = infiniteRepeatable(
            animation = tween(900),
            repeatMode = RepeatMode.Reverse,
        ),
        label = "pulse",
    )

    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(Color.Black.copy(alpha = 0.7f)),
        contentAlignment = Alignment.Center,
    ) {
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            Box(
                modifier = Modifier
                    .size((120 * pulse).dp)
                    .clip(CircleShape)
                    .background(
                        when (tapState) {
                            TapState.APPROVED -> Color(0xFF2E7D32)
                            TapState.DECLINED -> Color(0xFFC62828)
                            else -> MaterialTheme.colorScheme.primary
                        }
                    ),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    when (tapState) {
                        TapState.WAITING -> ")))"
                        TapState.APPROVED -> "OK"
                        TapState.DECLINED -> "X"
                        TapState.IDLE -> ""
                    },
                    color = Color.White,
                    fontSize = 32.sp,
                    fontWeight = FontWeight.Bold,
                )
            }
            Spacer(Modifier.height(24.dp))
            Text(
                when (tapState) {
                    TapState.WAITING -> "Hold card near phone"
                    TapState.APPROVED -> "Payment approved"
                    TapState.DECLINED -> "Payment declined"
                    TapState.IDLE -> ""
                },
                color = Color.White,
                fontSize = 20.sp,
            )
        }
    }
}

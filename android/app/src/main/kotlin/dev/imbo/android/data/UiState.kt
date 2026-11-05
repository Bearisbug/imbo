package dev.imbo.android.data

/** 每屏统一状态，互斥且穷尽，禁止用布尔量拼状态。 */
sealed interface UiState<out T> {
    data object Loading : UiState<Nothing>
    data class Error(val message: String) : UiState<Nothing>
    data class Content<T>(val data: T) : UiState<T>
}

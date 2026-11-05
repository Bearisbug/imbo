package dev.imbo.android.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.rules.TestWatcher
import org.junit.runner.Description

/** viewModelScope 跑在 Dispatchers.Main 上，单测里换成测试调度器。 */
class MainDispatcherRule(
    private val dispatcher: kotlinx.coroutines.CoroutineDispatcher = kotlinx.coroutines.test.UnconfinedTestDispatcher(),
) : TestWatcher() {
    override fun starting(description: Description) {
        Dispatchers.setMain(dispatcher)
    }

    override fun finished(description: Description) {
        Dispatchers.resetMain()
    }
}

/** mock 的默认延迟会拖慢单测，这里统一置零。 */
fun noLatency(): MockConfig = MockConfig(latencyMs = 0)

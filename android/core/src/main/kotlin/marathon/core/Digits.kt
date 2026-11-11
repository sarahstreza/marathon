// Typing a channel number on a number pad, shared by every input with
// digit keys (a TV remote, a keyboard). A port of
// crates/marathon-client/src/digits.rs.
//
// Digits are buffered; the number is committed as Action.GoToChannel when
// the input's Enter/OK key is pressed (commit) or DIGIT_TIMEOUT_MS passes
// without another digit (call tick by deadline). Inputs discard pending
// digits on any other key (clear).
package marathon.core

/** How long after the last digit a channel number is committed. */
const val DIGIT_TIMEOUT_MS: Long = 1500

/** Longest channel number that can be typed. */
private const val MAX_DIGITS = 4

/** Digits typed so far. See the file comment. Times are [Clock] milliseconds. */
class ChannelNumberEntry {
    private val digits = StringBuilder()
    private var lastDigitMs: Long? = null

    /**
     * Adds digit [d] (0–9) typed at [nowMs]. Digits past the fourth are
     * ignored, but still restart the timeout.
     */
    fun digit(d: Int, nowMs: Long) {
        if (d in 0..9 && digits.length < MAX_DIGITS) digits.append(d)
        lastDigitMs = nowMs
    }

    /** Commits the pending number, if any. */
    fun commit(): Action? {
        val number = digits.toString().toIntOrNull()
        clear()
        return number?.let { Action.GoToChannel(it) }
    }

    /** Discards pending digits. */
    fun clear() {
        digits.clear()
        lastDigitMs = null
    }

    /** When pending digits will be committed, if any are pending. */
    fun deadline(): Long? = lastDigitMs?.plus(DIGIT_TIMEOUT_MS)

    /** Commits pending digits if their timeout has passed. */
    fun tick(nowMs: Long): Action? {
        val deadline = deadline() ?: return null
        return if (nowMs >= deadline) commit() else null
    }

    /** The digits typed so far, for display. */
    fun pending(): String = digits.toString()

    fun isEmpty(): Boolean = digits.isEmpty()
}

// The overlay's state: what is on screen and how visible it is, driven by
// the Tuner's UiEvents. A port of the state half of
// apps/marathon-desktop/src/overlay/mod.rs (the drawing is the app's, in
// Compose). Thread-safe: the tuner thread feeds it, the UI thread reads it.
package marathon.core.overlay

import marathon.core.Banner
import marathon.core.Guide
import marathon.core.UiEvent

/** How long the banner stays up after a tune or ShowInfo. */
const val BANNER_HOLD_MS: Long = 4_000

/** How long an error toast stays up. */
const val TOAST_HOLD_MS: Long = 6_000

/** The overlay at one instant, for drawing. Contents are kept while they fade out. */
data class OverlayFrame(
    val banner: Banner?,
    val bannerOpacity: Float,
    /** A switch is under way and the new item is not playing yet (so its progress is unknown). */
    val tuning: Boolean,
    val guide: Guide?,
    val guideOpacity: Float,
    val toast: String?,
    val toastOpacity: Float,
    /** When the overlay next needs drawing. */
    val wake: Wake,
) {
    /** Whether anything is (even faintly) on screen. */
    val visible: Boolean get() = bannerOpacity > 0f || guideOpacity > 0f || toastOpacity > 0f
}

class OverlayModel {
    private var banner: Banner? = null
    private var tuning = false
    private val bannerFade = Fade()
    private var guide: Guide? = null
    private val guideFade = Fade()
    private var toast: String? = null
    private val toastFade = Fade()

    /** Applies a cue from the Tuner at [now]. */
    @Synchronized
    fun cue(cue: UiEvent, now: Long) {
        when (cue) {
            is UiEvent.Tuning -> {
                banner = cue.banner
                tuning = true
                showBanner(now)
            }
            is UiEvent.Playing -> {
                banner = cue.banner
                tuning = false
                if (cue.announce) showBanner(now)
            }
            UiEvent.Stopped -> tuning = false
            UiEvent.ShowInfo -> if (banner != null && !guideFade.isShown(now)) showBanner(now)
            UiEvent.Hide -> {
                bannerFade.hide(now)
                toastFade.hide(now)
            }
            is UiEvent.GuideChanged -> {
                val g = cue.guide
                if (g != null) {
                    guide = g
                    if (!guideFade.isShown(now)) guideFade.show(now, null)
                    // The guide says what everything is on; the banner would only cover it.
                    bannerFade.hide(now)
                } else {
                    guideFade.hide(now)
                }
            }
        }
    }

    /** Something went wrong; tell the viewer briefly. */
    @Synchronized
    fun toast(message: String, now: Long) {
        toast = message
        toastFade.show(now, TOAST_HOLD_MS)
    }

    /** Whether the guide is open (not fading out). */
    @Synchronized
    fun guideShown(now: Long): Boolean = guideFade.isShown(now)

    /** Whether the banner is up (not fading out). */
    @Synchronized
    fun bannerShown(now: Long): Boolean = bannerFade.isShown(now)

    @Synchronized
    fun frame(now: Long): OverlayFrame = OverlayFrame(
        banner = banner,
        bannerOpacity = bannerFade.opacity(now),
        tuning = tuning,
        guide = guide,
        guideOpacity = guideFade.opacity(now),
        toast = toast,
        toastOpacity = toastFade.opacity(now),
        wake = bannerFade.wake(now).min(guideFade.wake(now)).min(toastFade.wake(now)),
    )

    private fun showBanner(now: Long) {
        if (!guideFade.isShown(now)) bannerFade.show(now, BANNER_HOLD_MS)
    }
}

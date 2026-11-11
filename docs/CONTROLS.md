# Watching Marathon: controls

Marathon works like a TV. Each channel picks up where you left it, and
when an episode ends the next one starts on its own. Most of the time
you'll just flip channels and let it play. This page covers everything
else you can do, with a keyboard, a mouse, or a game controller.

All three work at the same time, and none of them takes priority:
whatever you pressed last wins. The same goes for programs controlling
Marathon over the network; see [REMOTE.md](REMOTE.md).

## Quick reference

| To… | Keyboard | Controller | Mouse |
|---|---|---|---|
| Change channel | PgUp / PgDn, or ↑ / ↓ | Right / left shoulder (RB / LB), or D-pad ↑ / ↓ | Scroll wheel |
| Go to a channel by number | Type the number | Use the guide | |
| See what's on | I, or Enter | Select (View / Back / Share) or A | Click |
| Open or close the guide | G | Y | |
| Skip back or forward 10 seconds | ← / → (hold to keep going) | D-pad ← / → (hold to keep going) | |
| Pause or resume | Space | Start (Menu / Options) | |
| Next episode | N | Right trigger (RT) | |
| Previous episode | P | Left trigger (LT) | |
| Subtitles on / off | S | X | |
| Fullscreen on / off | F | Click the left stick | |
| Close the guide or hide the banner | Esc | B | |
| Quit | Q | Home button (Xbox / PS) | |

Controller buttons are named by position. "A" means the bottom face
button, which is Cross (✕) on a PlayStation controller. "B" is the right
one (Circle ○), "X" the left one (Square □), and "Y" the top one
(Triangle △).

## What's on screen

Marathon stays out of the way. Most of the time you see only the video.

**The channel banner** runs along the bottom of the screen. It shows the
channel number and name, the episode (for example `S03E14 · The Injury`),
and a bar showing how far into the episode you are, with the time played
and total length. It appears as soon as you change channel, before the
new video has loaded, so you always know where you're headed. It also
appears when you ask what's on, then fades away after a few seconds. It
doesn't appear when one episode simply rolls into the next. If you pause
while it's up, it says **Paused**.

**The guide** lists every channel down the left side with what each one
is showing right now. The channel you're watching has its number in
orange. The guide stays open until you choose a channel or close it.

**Messages** appear briefly in the top-right corner when something goes
wrong, for example when a video file can't be played.

**The mouse pointer** hides after three seconds without mouse activity,
and comes back as soon as you move the mouse.

## Changing channels

**Flip up and down** with PgUp / PgDn, the ↑ / ↓ arrows, the controller's
shoulder buttons or D-pad, or the mouse wheel. Channels wrap around: up
from the highest channel takes you to the lowest. Channels with nothing
to play are skipped.

On a touchpad or a free-spinning mouse wheel, one flick changes channel
once. Scroll again to keep going.

**Jump straight to a channel** by typing its number on the keyboard, for
example `1` `2` for channel 12. Marathon tunes in when you press Enter,
or after a second and a half without another digit. Pressing any other
key cancels the number. The number you type isn't shown on screen yet,
and if there's no channel with that number, nothing happens.

**Use the guide** to choose a channel from a list, which is the easiest
way with a controller:

1. Press **G** (controller: **Y**) to open the guide.
2. Move the highlight with **↑ / ↓** (controller: **D-pad**). It wraps
   from the bottom of the list to the top.
3. Press **Enter** (controller: **A**; mouse: **click**) to tune in. The
   guide closes.

To close the guide without changing channel, press **Esc** or **G**
again (controller: **B** or **Y**).

While the guide is open, the arrow keys and D-pad move the highlight
instead of changing channel, and ← / → do nothing. Everywhere else they
work as described on this page.

### Where a channel resumes

Each channel remembers where you left it. When you come back, it starts
again five seconds before that point, so you can pick up the thread. If
you'd nearly finished an episode (within the last 30 seconds or so), it
moves on to the next one instead.

## While watching

| To… | Keyboard | Controller |
|---|---|---|
| Pause or resume | Space | Start |
| Skip back 10 seconds | ← | D-pad ← |
| Skip forward 10 seconds | → | D-pad → |
| Next episode | N | Right trigger |
| Previous episode | P | Left trigger |
| Show what's on | I or Enter | Select or A |
| Hide the banner | Esc | B |

Hold ← or → (or the D-pad's left or right) to keep skipping. No other
key or button repeats when held, so you can't flip through every channel
by accident.

"Next" and "previous" episode follow the channel's own order. On a
shuffled channel, "previous" goes back to whatever played before, which
may not be the previous episode of the show. Skipping moves the channel
along for good: next time you tune in, it resumes from there.

**Subtitles:** S (controller: X) turns them on and off. If a video has
several subtitle tracks, each press moves to the next track, and then
off. If a video comes with a subtitle file, it's switched on
automatically.

**Fullscreen:** Marathon starts fullscreen. F (controller: click the left
stick) switches between fullscreen and a window.

## When something won't play

If an episode can't be played (say the file was renamed or deleted),
Marathon shows a short message and moves on to the next one. It keeps
the channel going rather than stopping. If five items in a row fail, it
stops trying and tells you. Change channel, or press N (controller:
right trigger), to try again.

## Quitting

Press **Q** (controller: the Home / Xbox / PS button), or close the
window. Marathon saves where you were on the current channel and will
start there next time. To start on a particular channel instead, run it
with `--channel N`.

## Game controllers

Most controllers work, including Xbox, PlayStation, Switch Pro, and
generic USB or Bluetooth pads. You can plug them in or unplug them while
Marathon is running, and several can be connected at once.

If your controller does nothing:

- **Linux:** Marathon needs permission to read input devices. Desktop
  distributions usually allow this for the logged-in user. Marathon logs
  a warning (`gamepads unavailable`) at startup if it can't.
- **Steam:** if Steam is running, it may take over the controller
  ("Steam Input") and hold back some buttons, especially the Home button.
  Turn Steam Input off for Marathon, or quit with Q instead.
- **No mapping:** an unusual controller may have no known button layout.
  Marathon logs `No mapping found` for it. Such controllers can usually be
  given a layout with the `SDL_GAMECONTROLLERCONFIG` environment
  variable, the same way as for games.

Builds made without gamepad support (`--no-default-features`) ignore
controllers entirely.

## Coming later

- **HDMI-CEC:** using your TV's own remote (milestone M7).
- **Remote control over the network:** phones, home automation, and
  scripts (milestone M6).

Everything you can do from the keyboard will be possible through those
too.

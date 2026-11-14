# Watching Marathon: controls

Marathon works like a TV. Each channel picks up where you left it, and
when an episode ends the next one starts on its own. Most of the time
you'll just flip channels and let it play. This page covers everything
else you can do, with a keyboard, a mouse, a game controller, or the
TV's own remote.

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

## The TV's remote (HDMI-CEC)

If Marathon's computer is plugged into the TV and both support HDMI-CEC
(most TVs do, under a brand name like Anynet+, Bravia Sync, SimpLink, or
just "HDMI control"), the TV's own remote can control Marathon. Marathon
shows up in the TV's list of inputs as **Marathon**.

This needs a build with CEC support (`--features input-cec`) and the
`--cec` option. It also needs a way onto the CEC bus: a Raspberry Pi has
one built in, and most PCs need a Pulse-Eight USB-CEC adapter between
the computer and the TV.

| To… | TV remote |
|---|---|
| Change channel | Channel + / −, or ▲ / ▼ |
| Go to a channel by number | Type the number (then OK, or wait) |
| See what's on | Info (or OK) |
| Open or close the guide | Guide, Menu, or Channel list |
| Skip back or forward 10 seconds | ◀ / ▶ (hold to keep going), or ⏪ / ⏩ |
| Pause or resume | ⏯, ▶ (play), ⏸ (pause); ⏹ (stop) pauses too |
| Next / previous episode | ⏭ / ⏮ |
| Close the guide or hide the banner | Back, Return, or Exit |

Which buttons a TV passes on varies by make. The arrows, OK, and Back
nearly always work; Info and Guide often don't. In the guide, the arrows
and OK work just like the keyboard's.

The colour buttons do nothing unless you choose something for them, for
example `--cec-button red=toggle_subtitles --cec-button
blue=toggle_guide`. Any action from [REMOTE.md](REMOTE.md) works, by name
or as JSON.

**Switching inputs.** Marathon is meant to be a polite guest on your TV:

- Turning the TV on never switches it to Marathon. It stays on whatever
  input it was on.
- When you switch the TV to another input, or turn the TV off, Marathon
  pauses and saves its place. When you switch back to Marathon, it
  carries on (start it with `--cec-no-resume` to have it wait for you to
  press play instead).
- Marathon never turns the TV on or off. The one exception is
  `--cec-claim-on-start`, which makes the TV switch to Marathon (turning
  it on if it's in standby) when Marathon starts.

If the adapter isn't plugged in when Marathon starts, or gets unplugged,
Marathon keeps playing and tries again every few seconds. Everything
else keeps working in the meantime.

**If the remote does nothing:**

- On Linux, Marathon needs permission to use the adapter: `/dev/cec0`
  (built-in CEC, usually group `video`) or `/dev/ttyACM0` (a Pulse-Eight,
  usually group `dialout`). Marathon logs `no permission to use the CEC
  adapter` if that's the problem.
- Check that HDMI-CEC is switched on in the TV's settings.
- Some adapters can't tell which HDMI input they're plugged into. Tell
  Marathon with `--cec-hdmi-port N`.
- If there are several adapters, pick one with `--cec-adapter PATH`.
- Run with `RUST_LOG=info,libcec=debug` to see what's happening on the
  CEC bus.

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

## On Android (phones and Android TV)

The Android app watches a Marathon server on your network: the first
time it starts, give it the server's address (for example
`http://192.168.1.10:7878`) and its token, if it has one. Everything
else works as on the desktop. Channels pick up where any device left
them, so you can start an episode on the computer and finish it on the
phone.

**With a TV remote** (Android TV, Google TV) or a keyboard:

| To… | Remote |
|---|---|
| Change channel | Channel + / −, or ▲ / ▼ |
| Go to a channel by number | Type the number (then OK, or wait); it shows top left as you type |
| See what's on | Info, Menu, or OK |
| Open or close the guide | Guide |
| Skip back or forward 10 seconds | ◀ / ▶ (hold to keep going), or ⏪ / ⏩ |
| Pause or resume | ⏯, ▶ (play), ⏸ (pause); ⏹ (stop) pauses too |
| Next / previous episode | ⏭ / ⏮ |
| Subtitles on / off | Captions (CC) |
| Close the guide or hide the banner | Back |

A keyboard's letters (N, P, I, G, S) and a game controller's buttons
work as in the tables above.

**On a phone's touch screen:**

| To… | Touch |
|---|---|
| Change channel | Swipe up / down |
| Skip back or forward 10 seconds | Double-tap the left / right third of the screen |
| Pause or resume | Double-tap the middle |
| See what's on, and show the buttons | Tap once |
| Guide, previous / next episode, pause, subtitles, audio only, change server | The buttons along the top |
| Close the guide or hide the banner | Back |

**Back** closes whatever is open; with nothing open, it leaves the app.
Leaving the app (Back, Home, switching apps, or the screen turning off)
pauses the channel and saves its place. Coming back resumes it, unless
you had paused it yourself. If another app starts playing sound, or you
unplug your headphones, Marathon pauses.

**Audio only** (a button along the top) turns off the picture and plays
just the sound, using much less data. It keeps playing when you leave the
app or the screen turns off, like a radio. The notification and lock
screen have channel down, pause, and channel up, and "Stop listening"
pauses and puts it away. Tap **Video** to get the picture back. The
server needs ffmpeg for this. Without it, the button says so.

## Coming later

- **HDMI-CEC:** using your TV's own remote (milestone M7).
- **Remote control over the network:** phones, home automation, and
  scripts (milestone M6).

Everything you can do from the keyboard will be possible through those
too.

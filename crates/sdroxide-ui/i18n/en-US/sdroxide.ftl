# sdroxide — the interface's English text, the reference every translation
# is checked against. See crates/sdroxide-ui/src/i18n.rs for the rules.
#
# Ids are grouped by where they are shown, and named after it:
# settings-ui-* is the UI tab of the Settings window, and so on. A message
# shown in capitals is written in capitals here; the code does not upper-case
# translated text.

## The language's own name, as the language picker lists it.
language-name = English

## Values from sdroxide-types, as the screen names them (src/i18n/labels.rs).

layout-mode-auto = Auto
layout-mode-desktop = Desktop
layout-mode-tablet = Tablet
layout-mode-small = Small screen
layout-mode-phone = Phone

ui-theme-default = Default
ui-theme-light = Light
ui-theme-high-contrast = High contrast
ui-theme-green-phosphor = Green phosphor
ui-theme-amber-phosphor = Amber phosphor
ui-theme-teal-orange = Teal / orange
ui-theme-rainbow = Rainbow
ui-theme-nord = Nord
ui-theme-nord-dark = Nord dark
ui-theme-gruvbox = Gruvbox
ui-theme-everforest = Everforest
ui-theme-solarized-dark = Solarized dark
ui-theme-solarized = Solarized
ui-theme-dracula = Dracula
ui-theme-catppuccin-mocha = Catppuccin mocha
ui-theme-catppuccin-latte = Catppuccin latte
ui-theme-modern-minimalist = Modern minimalist

chrome-style-angled = Angled
chrome-style-rectangular = Rectangular
chrome-style-rounded = Rounded
chrome-style-gradient = Gradient
chrome-style-bevel = 3D bevel
chrome-style-terminal = Terminal

font-size-small = Small
font-size-medium = Medium
font-size-large = Large

bandplan-kind-ham = Ham
bandplan-kind-cw = CW
bandplan-kind-digi = Digital
bandplan-kind-phone = Voice
bandplan-kind-beacon = Beacon
bandplan-kind-broadcast = Broadcast
bandplan-kind-am = AM / LW / MW
bandplan-kind-cb = CB

speech-verbosity-terse = Terse
speech-verbosity-normal = Normal
speech-verbosity-full = Full

# The examples in brackets are what the (English) voice says.
speech-freq-style-dial = Dial (14 point 074)
speech-freq-style-digits = Every digit
speech-freq-style-full = Dial with units

speech-callsign-style-phonetic = Phonetic (kilo one alpha)
speech-callsign-style-letters = Letters (kay one ay)
speech-callsign-style-as-is = As written

## Settings window, UI tab (src/app/settings/ui_tab.rs).

settings-ui-display = Display
settings-ui-moved-note = Panadapter detail, the spectrum's reaction and the waterfall's scroll speed are set from the SPEC button on the display strip — the DISP menu on a narrow window.
settings-ui-language = Language
    .hint = The language of the interface. The manual behind F1 and the voice announcements are in English whatever is picked here.
settings-ui-language-system = System ({ $language })
settings-ui-layout = Layout
settings-ui-theme = Theme
settings-ui-button-style = Button style
settings-ui-window-style = Window style
settings-ui-frame-rate = Screen update rate
settings-ui-fps = { $fps } fps
settings-ui-palette = Waterfall palette
settings-ui-tuning-buttons = Tuning buttons
settings-ui-tuning-buttons-show = Show on phone and tablet
    .hint = A row of step-down / step / step-up buttons under the control strip on a touched screen. There is no wheel to scroll a digit with and no dial, so without them the only way to move a known step is to type the whole frequency in. Never drawn on a desktop.
settings-ui-tuning-step = Step: { $step }
settings-ui-tuning-snap = First press snaps to the step
    .hint = A dial left between two multiples of the step goes to the next one in the direction pressed — 7 074 300 goes up to 7 075 000 at a 1 kHz step, down to 7 074 000. After that the buttons move by the step as usual. Off, every press moves by exactly the step.
settings-ui-smoothing = Waterfall smoothing
settings-ui-smoothing-interpolate = Interpolate
    .hint = Blend each screen pixel with the bins and rows around it, so a signal looks continuous where the display is wider than the transform. Turn it off for a rectangular waterfall — one block per bin, one per row — which is what reading a signal's shape off the picture needs: an interpolated signal cannot be told apart from a genuinely wider one. A bigger FFT (the FFT chip) is the other half of that.
settings-ui-clock = Clock
settings-ui-clock-show = UTC + local on the panadapter
    .hint = A wall clock at the top-right of the panadapter, in UTC and your own time.
settings-ui-background = Spectrum background
settings-ui-background-gradient = Gradient
settings-ui-background-top = top
settings-ui-background-bottom = bottom
settings-ui-spot-colours = Spot label colours
    .hint = The tint each spot source wears: the boxes along the bottom of the waterfall, the badges in the SPOTS list and the dots on the world map.
settings-ui-spot-colour = Colour for { $kind } spots
settings-ui-spot-colours-reset = Put every spot colour back to its default
settings-ui-reset = Reset
settings-ui-bandplan-colours = Band plan colours
    .hint = The shade each class of allocation is painted in on the band-plan strip along the bottom of the waterfall. The blocks are drawn semi-transparent over the waterfall, so they land darker there than in the swatch here.
settings-ui-bandplan-colour = Colour for { $kind } allocations
settings-ui-bandplan-colours-reset = Put every band-plan colour back to its default
settings-ui-skimmer-font = Skimmer font size
settings-ui-waterfall-font = Waterfall / spectrum font size
settings-ui-interface-font = Interface font size
    .hint = Scales the whole interface — menus, dialogs, windows, the radio tabs, the top bar and its buttons. The two sizes above are relative to it.
settings-ui-cities = Cities on maps
    .hint =
        Draw the world's cities — a dot per place, with its name beside it where there is room — on the flat maps: FT8/WSPR, APRS, ADS-B and AIS.

        They are most of what says *where* a dot is on a map of the whole world, and they are also the busiest thing on it. Turn them off if the names are getting in the way of the stations you are reading. The 3D globe is unaffected — its cities are night-side lights rather than markers, and nothing there is written across a contact.
settings-ui-cities-show = show cities and their names
settings-ui-update-check = Check for updates at startup
    .hint = Asks sdroxide.com once per start whether a newer release has been published, and says so in a dismissable banner above the waterfall. Nothing is sent beyond the request itself.
settings-ui-3d-view = 3D view
settings-ui-clouds = Cloud rendering
settings-ui-clouds-layered = Layered
settings-ui-clouds-volumetric = Volumetric
settings-ui-clouds-note = How the CLOUDS layer in the 3D view draws the weather. Layered stacks slices through the troposphere and is the cheap option. Volumetric walks a ray through it instead, so the Sun casts the cloud tops onto the deck below and lightning glows out through the storm making it rather than only brightening its outside — at several times the cost per pixel.

## Settings window, UI tab: voice announcements.

speech-title = Voice announcements
speech-enabled = Speak changes to the radio
    .hint = Reads out what changed, so the radio can be operated without seeing it
speech-voice = Voice
speech-voice-shipped = Shipped voice
speech-speed = Speed
    .hint = The voice stretches or compresses its own phrasing, so the pitch does not change. Past about 2× it stops getting shorter.
speech-volume = Volume
speech-output = Output
speech-detail = Detail
speech-duck = Duck receiver
speech-duck-while = While speaking
speech-test = Test
speech-what = What to announce
speech-cat-frequency = Frequency
speech-cat-mode-band = Mode and band
speech-cat-vfo-split = VFO and split
speech-cat-agc-gain = AGC and gain
speech-cat-levels = Drive, tune and mic
speech-cat-ptt = Transmit and receive
speech-cat-rit-xit = RIT and XIT
speech-cat-memory-scan = Memories and scanning
speech-cat-band-edge = Leaving an amateur band
speech-cat-notices = Warnings and messages
speech-cat-filters = Filters, squelch and noise reduction
    .hint = Off by default: these move constantly while chasing a signal
speech-decoded = Decoded messages
speech-ft8-to-me = FT8 calls to me
speech-ft8-cq = FT8 CQs I could answer
    .hint = A busy evening on twenty metres is a hundred of these a minute
speech-js8 = JS8 messages to me
speech-js8-allcall = JS8 @ALLCALL too
speech-fsq = FSQ messages to me
speech-include-snr = Include the report
    .hint = Only where the message carries none of its own — a decode that already reports a number does not also get ours
speech-ft8-qso = My own FT8 exchange
    .hint = What the sequencer is about to send, and the contact ending
speech-reading-text = Reading decoded text aloud
speech-text-cw = CW
speech-text-rtty-psk = RTTY, PSK, Olivia, THOR, FSQ
speech-text-cw-locked = CW only while the decoder is locked
    .hint = Reading an unlocked decoder's output is worse than silence
speech-text-note = Both are off by default. A decoder produces text faster than speech reads it, so anything that falls too far behind the live audio is dropped rather than queued.
speech-tuning-up = Tuning up
speech-swr-while-tuning = Read the SWR out while TUNE is held
speech-swr-every = Every
speech-swr-summary = Report the best match on release
speech-swr-alarm = Warn about high SWR during any transmission
speech-how-read = How things are read
speech-frequencies = Frequencies
speech-callsigns = Callsigns
speech-quiet-on-tx = Stay quiet while transmitting
    .hint = Speech goes to your speakers, and therefore into your microphone. High-SWR warnings still get through.
speech-device-note = Announcements play on their own sound device, so they are never recorded and never sent to anyone listening remotely. Keys for speaking the status, repeating the last announcement and stopping mid-sentence are on the Controls tab.

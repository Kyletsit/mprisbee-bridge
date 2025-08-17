# mprisbee-bridge
A bridge between [mb_MPRISBee](https://github.com/Kyletsit/mb_MPRISBee) MusicBee plugin and DBus MPRIS.

First start mprisbee-bridge, then MusicBee in wine.

A simple launch script could look like this:
```
#!/bin/bash

export WINEPREFIX="$HOME/Programs/MusicBee/"

mprisbee-bridge &
wine "$WINEPREFIX/drive_c/Program Files/MusicBee/MusicBee.exe" &
```

### Options:
- -n | --notifications
  - send notifications on track change

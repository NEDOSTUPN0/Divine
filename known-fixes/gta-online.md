# GTA Online on Linux

Since Rockstar added BattlEye to GTA V, GTA Online no longer officially works on Linux. There is still a way to get part of it working: invite only and friends only sessions. Public sessions do not work.

## What you need

1. The BattlEye runtime. Divine turns it on automatically for GTA V and GTA V Enhanced, unless you changed that setting yourself. You can check it in the game's settings, under Anti-cheat.
2. These lines in `/etc/hosts` (editing it needs root):

```
0.0.0.0 paradise-s1.battleye.com
0.0.0.0 test-s1.battleye.com
0.0.0.0 paradiseenhanced-s1.battleye.com
```

## Getting into a session

Start in Story Mode first, and from there switch to an invite only or friends only session.

Going online straight from the main menu does not work: right after you join, you are sent back to the main menu with "You were kicked from session by BattlEye".

## Is it safe?

As far as I know, nobody has been banned for this. Still, use it at your own risk.

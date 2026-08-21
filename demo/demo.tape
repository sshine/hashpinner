Output "demo/demo-raw.gif"

Set Shell "bash"
Set Theme "Catppuccin Mocha"
Set FontSize 16
Set Width 1200
Set Height 700
Set Padding 24
Set TypingSpeed 55ms
Set CursorBlink false

Hide
Type "source demo/setup.sh"
Enter
Sleep 2s
Type "clear"
Enter
Sleep 500ms
Show

Sleep 1s
Type "cat .github/workflows/ci.yml"
Enter
Sleep 4s

Hide
Type "clear"
Enter
Sleep 500ms
Show

Type "hashpinner --check --no-allow"
Enter
Sleep 4s

Hide
Type "clear"
Enter
Sleep 500ms
Show

Type "hashpinner --pin"
Enter
Sleep 14s

Hide
Type "clear"
Enter
Sleep 500ms
Show

Type "cat .github/workflows/ci.yml"
Enter
Sleep 5s

Hide
Type "clear"
Enter
Sleep 500ms
Show

Type "hashpinner --check --deep --no-allow"
Enter
Sleep 14s

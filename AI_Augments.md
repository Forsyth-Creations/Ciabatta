1. Text is being clipped from the TUI of `ciabatta ai`. Please correct this. Instead of making it one scroll window, you can allow the whole screen to scroll, but leave the input bar at the bottom of the screen

2. The AI has been seen to change unrelated files. Make sure to run a check to see if the file it wants to change is relivant to the current architecture and the user's current task. If it is not, prompt the user for confirmation before making any changes.

3. Continue showing command suggestions when the / is present at the front of the input bar. This way, the format remains up for the command the user is putting in

4. Make sure that the AI can actually use teh local tooling. I ran into an issue today where it couldn't run "yarn"

5. Make sure the AI remembers when it used local tooling. Usually this will be for builds, lints, formats, etc

6. When the AI makes a change to a file, it should also update the mind map to reflect that change. This will help keep the architecture mapping accurate and up-to-date.


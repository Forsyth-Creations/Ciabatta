Here's that needs to happen:

1. Design a small server called "AI assistant". This daemon is how you will interact with the AI model
2. Keep track of a small per-project folder in the .ciabatta cache. In this AI folder, you will have a JSON that you will update. At a minimum, it will have a "confidence" score, from 1 to 100, for overall AI ability. This will be losey trained by the user. The less files you choose with the highest accuracy, in the correct quantity over time, the better the score. A low score is considered a junior dev, while a high score is considered a senior dev. The score will be updated based on the user's interactions and feedback with the AI assistant.
3. As you traverse files, tag them as connecting to a certain architecture. A file can have multiple tags. These tags will help with loopup later. Use the AI to help create these baselines tags
4. As you traverse files that connect to certain architecture, ask for confirmatiom from the user. Build a "map" of files to architecture. The more a file is used, the more the path score from the architecture should increase. You should end up with a mapping of files as your traverse them. Keep track of your knowledge of an architecture as you go
5. I should be able to see the graph in real time in a browser to keep track of your progress
6. Be able to use standard tools like grep, ack, and ripgrep to search through files and gather information about the codebase. The AI assistant should be able to suggest relevant files based on the search results and the architecture tags.
7. Implement a feedback loop where the user can provide feedback on the AI assistant's suggestions and actions. This feedback will be used to improve the AI's performance and accuracy over time.
8. In the ciabatta config, I should be able to configure any number of base images that the user can spin up in either podman or docker (configured in the ciabatta config). This way, the AI can spin up a safe space and work
9. The AI should use the mind map to better inform it where to look. Grep is useful for finding "everything", the mind map is specifically for finding "excatly what is needed". 
10. Make sure to use Ratatui for the TUI side
11. Make this provider-agnostic. When first configuring it, I can either use Claude or point it to an OpenAI endpoint
12. AI should be able to use basic, local tooling
13. Make sure to allow a flag for tls-verify being false for the OpenAI endpoint, in case the user is using a self-signed certificate or a local development environment.

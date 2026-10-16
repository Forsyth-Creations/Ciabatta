# Ciabatta Daemon

The daemon is designed to keep all of your environment variables consistent with how Ciabatta wants to handle things. The idea is simple: the daemon runs in the background. You tell the daemon what env your using for ciabatta (.env.production, .env.development, .env.local), and it makes sure you always have those environment variables sourced. As soon as you leave the workspace, it should pop all those variables so you the rest of your system doesn't even know they are there

# Why this is in Ciabatta

The goal of this is to give Ciabatta a fighting chance in knowing where everything is for a build. From the env variables to the versions of packages, Ciabatta will know exactly what changes between builds and how to recreate the failure exactly. 

Additionally, if an environment variable changes when you pull the new software, we can notify the user of this change. For instance, if you thought you were using one container, nginx:1.31, but recently the env switched to nginx:1.32, you'd want to know! We should also support background tasks that are set to handle when env variables change, and notify the user that these tasks are running too

Perhaps we can take a page out of the terraform playbook. You can use a standard .env file format, or you can use a format that looks like the following:

value "public_url" {
  source = env
  default = "testing.com"
}

"env" could be local env, or aws secrets

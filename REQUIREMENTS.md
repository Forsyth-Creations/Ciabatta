You're going to write a Rust application called "ciabatta". Here's what it does:

- Ciabatta is a command line tool for publishing artifacts. Every publication is a "recipie"
- There are some pre-defined recipies for Nexus, S3, Artifactory, and Docker registries (Like docker and ECR)
- Ciabatta will always look in a .ciabatta directory for recipies, and will allow users to define their own custom recipies in that directory.
- The tui should be written in ratatui, with Rust
- Ciabatta should allow me to specify multiple recipies to run in a single command, and it should run them in parallel and show me progress
- I should be able to pass environment variables to the recipies, and they should be available to the recipies when they run
- The TUI should have a fun ASCII art logo for ciabatta, and a nice progress bar for each recipie that is running
- Ciabatta should automatically pull in information about the branch, commit, tag, and build number from the environment variables that are set by the CI/CD system (like Jenkins, GitHub Actions, etc.). You can define your system in the toml file, and ciabatta will automatically pull in the correct environment variables for that system.
- Enviroment variables directly passed via the command line should always supersede the ones pulled in from the CI/CD system.
- When Ciabatta pulled the variables from the CI/CD, it should print them out to the console. Additonally, when used in the internal system, they will be names "CIABATTA_BRANCH", "CIABATTA_COMMIT", "CIABATTA_TAG", and "CIABATTA_BUILD_NUMBER". This way, if the user wants to specify them via the command line, they can do so with the same names.
- The user should be able to specify that they want to use a bash script. In which case, you can explicitly run that bash script. Make the Ciabatta variables available to the bash script as environment variables. The bash script should be able to use the variables in the same way as the recipies.
- Also in the .ciabatta directory is a toml file. It defines usage of the recipies. Here's an example:

```toml
[system]
ci = "gitlab"
containers = "docker" # could be docker or podman

[registries.nexus]
url = "https://nexus.example.com/repository/maven-repository/"
tls_verify = true
needs_auth = true
login_script = "./nexus_login.sh"

[registries.docker]
url = "https://docker.example.com/"
tls_verify = true
needs_auth = true
login_script = "./docker_login.sh"
[registries.s3]
url = "https://s3.example.com/"
tls_verify = true
needs_auth = true

[registries.ecr]
url = "https://ecr.example.com/"
tls_verify = true
needs_auth = false

[recipies.release_frontend]
registry = "nexus"
local_artifact_path = "frontend/dist"
publish_path = "frontend/{CIABATTA_BRANCH}/{CIABATTA_COMMIT}/frontend"

[recipies.release_backend.push]
bash_script = "scripts/release_backend.sh"

[recipies.release_backend.pull]
bash_script = "scripts/pull_backend.sh"


```

- The root directory of Ciabatta is always one above the .ciabatta directory. This is where the artifacts will be published from, and where the recipies will look for files to publish.
- The user should be able to use the CLI to figure out how to write the config file. It should provide them useful information about the structure of the config file, and what options are available for each registry and recipie.
- The user should be able to run a dry-run of the recipies, which will show them what would happen without actually publishing anything. This is useful for testing and debugging.
- Ciabatta should be able to be build and run on Linux, MacOS, and Windows. It should be distributed as a single binary for each platform.
- The build needs to happen over CI
- Ciabatta should have it's own frontend, which we will host through GitHub Pages. The frontend should provide a nice interface for users to view the recipies, their status, and logs of previous runs. It should also allow users to trigger new runs of recipies from the web interface.
- The Ciabatta frontend can be built with Vite
- If there is an env varaible listed in a recipie publish path, but it is not specified/avaiblae, error immedaitely
- Login should be handled by the recipies themselves, and not by Ciabatta. Ciabatta should just pass the environment variables to the recipies, and they should handle the login process.
- in the .ciabatta directory, you should be able to write scripts to handle the login process for each registry. These scripts should be called by the recipies when they need to login, and they should be able to use the environment variables passed to them by Ciabatta. Not every registry should 
- The common types of registries we support are Nexus, S3, Artifactory, Docker, and ECR
- If a login script is not specified for a registry, Ciabatta should assume that the registry does not require login, and it should not attempt to login
- Since ciabatta knows the structure of the repos/where to pull from, I should be able to also "pull" artifacts from these registries
- Write the CI to release both the crate as well as the browser. The website should give options to download the binary for each platform, as well as the source code. The website should also provide instructions
- Use good structures and enums for the recipies and registries, so that it is easy to add new ones in the future. The code should be well organized and easy to read, with clear separation of concerns between the different components of the application.
- Give me the ability to define  a recipie using the cli interactively

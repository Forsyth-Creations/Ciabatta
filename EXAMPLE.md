This repo should have a command called "init". The init function will generate an example repo with all the bells and whistles: multiple sub-workspaces, scripts, dependencies, and a ciabatta.toml file. This will serve as a template for users to understand how to structure their own monorepo and utilize Ciabatta effectively. The generated example repo will include.

Additionanally, there should be no distinction between "workflow" and "run". The "run" should be able to run a collection of scripts, and resolve the graph appropriately. Add a "filter" parameter to the "run" command, which will allow users to specify a subset of scripts to run based on tags or other criteria. This will provide flexibility in executing specific parts of the workflow without having to run the entire set of scripts, making it easier to test and debug individual components of the monorepo.

Additionally, the UI should NOT take arbitraty commands as a text input. Remove that. 

Add some options to INIT as well to give the user proper examples: nexus, docker, multi-sub-workspace "run" examples, etc. The init command should also include a README.md file that explains the structure of the generated example repo, how to use Ciabatta, and best practices for managing a monorepo. This will provide users with a comprehensive guide to getting started with Ciabatta and understanding the benefits of using a monorepo approach.


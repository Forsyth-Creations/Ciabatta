# The Analyze Feature

Figuring out how a repo is structured can be difficult. The Analyze feature is designed to help you understand the structure of a repository by providing insights into its contents, dependencies, and overall organization.

A good codebase should have the following:

- External dependencies (crates.io, npm, pip, dockerhub, etc.) 
- Internal dependencies (other modules or packages within the same repository)
- Publish points (where artifacts are published, such as a package registry or container registry)

When you run `ciabatta analyze`, it will analyze the codebase as a JSON file that maps dependencies. This JSON is then rendered at localhost:8080, where you can view the structure of the repository in a visual format. The dependency graph is interactive, allowing you to click on nodes to see more information about each dependency, including its version, license, and any known vulnerabilities. The nodes should be left to right: dependencies, internal dependencies, and publish points. The edges should be top to bottom: dependencies, internal dependencies, and publish points.

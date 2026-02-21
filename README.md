# Plank
Generate a Dockerfile easily from many packages

This tool has been built to simplify the creation of a Dockerfile in a monorepo, by discovering all packages
and processing them to so that there is one build layer and one run-time layer per package.

This project began by performing this for ROS2 packages, however the `package.xml` format is generic, and no dependency
on the standard tooling is taken.

# Usage
As an example to build a ROS2 Dockerfile:
Run `plank --base-image ros:jazzy --default-resolver "apt update && apt install {}" --build-command "colcon build"`

# Future Work
Many steps are yet to be taken to make this a broadly useful tool:
- [ ] Add examples for existing large repositories, such as [Autoware](https://github.com/autowarefoundation/autoware)
    While this project is not a monorepo, ROS2 tooling often tries to make it feel that way with auto-cloning and 
    automatic package discovery. Autoware is an example of a complex piece of software with many moving parts, and part
    of the desire of `plank` is to make it easier to understand the build pipeline, and to work with a large amount of
    code without too much time spent watching `docker build`.
- [ ] Enable the use of more features of Docker
    It should be possible to specify your own layers to be added in a clean and consistent manner, both `run` and
    `copy` but also things like `add`. I suspect this looks like adding [jinja](https://github.com/mitsuhiko/minijinja)
    templating and making many custom variables available, such as package name, direct dependencies etc.
- [ ] Test this more in non ROS2 circumstances
- [ ] Explore this more in relation to other monorepos
    There are many other tools for managing many packages in a monorepo, and it could be interesting to explore how a
    tool like this could function in that environment. Do we dedicate to the `package.xml` format, or try and make it
    easy to utilise their formats? This itself is a large question, but manual Dockerfile creation when things already
    form a nice graph can be a chore.

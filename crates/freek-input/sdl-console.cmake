# Read by SDL3's CMake build on Linux (see .cargo/config.toml). The game
# uses SDL only for controllers (the window is winit's), so SDL is built
# without its X11/Wayland video drivers; otherwise its build stops on
# systems without their development headers.
set(SDL_UNIX_CONSOLE_BUILD ON CACHE BOOL "SDL without X11/Wayland video")

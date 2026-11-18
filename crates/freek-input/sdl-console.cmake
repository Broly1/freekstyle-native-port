# Read by SDL3's CMake build on Linux (see .cargo/config.toml). The game
# uses SDL only for controllers (the window is winit's, SDL_INIT_VIDEO is
# never used), so SDL is built without its X11/Wayland/KMSDRM video
# drivers. Otherwise its build stops on systems without their development
# headers, or with only some of them (Debian with libx11-dev but not
# libxss-dev: "Couldn't find dependency package for XSCRNSAVER").
# FORCE: a build folder left by an earlier failed run keeps its cached
# values, which a plain CACHE set does not replace.
set(SDL_UNIX_CONSOLE_BUILD ON CACHE BOOL "SDL without X11/Wayland video" FORCE)
set(SDL_X11 OFF CACHE BOOL "no X11 video driver" FORCE)
set(SDL_WAYLAND OFF CACHE BOOL "no Wayland video driver" FORCE)
set(SDL_KMSDRM OFF CACHE BOOL "no KMS/DRM video driver" FORCE)

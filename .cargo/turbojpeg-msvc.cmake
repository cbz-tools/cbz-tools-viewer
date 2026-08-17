# Applied through CMAKE_TOOLCHAIN_FILE to the libjpeg-turbo vendor build.
# On Windows/MSVC, libjpeg-turbo uses WITH_CRT_DLL to select the DLL CRT
# (/MD, or /MDd for Debug), keeping its static library compatible with Rust.
# The cache setting is intentionally limited to Windows so other platforms are
# unaffected by this MSVC-specific workaround.
if(WIN32)
  set(WITH_CRT_DLL ON CACHE BOOL "Use the DLL CRT for the Windows/MSVC libjpeg-turbo vendor build" FORCE)
endif()

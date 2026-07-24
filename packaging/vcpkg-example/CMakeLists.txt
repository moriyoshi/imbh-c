cmake_minimum_required(VERSION 3.16)
project(imbh_c_example LANGUAGES C)

# Configure with the vcpkg toolchain so manifest mode installs imbh-c from the registry:
#   cmake -S . -B build -DCMAKE_TOOLCHAIN_FILE=$VCPKG_ROOT/scripts/buildsystems/vcpkg.cmake
find_package(imbh-c CONFIG REQUIRED)

add_executable(example example.c)
target_link_libraries(example PRIVATE imbh-c::imbh-c)

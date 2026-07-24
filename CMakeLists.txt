# Build imbh-c (the Rust cdylib/staticlib) via cargo and expose it as an imported CMake target, plus
# the C and C++ quickstart examples wired into CTest.
#
#   cmake -S . -B build && cmake --build build && ctest --test-dir build --output-on-failure
cmake_minimum_required(VERSION 3.16)
project(imbh_c LANGUAGES C CXX)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

find_program(CARGO cargo REQUIRED)

set(IMBH_C_ROOT ${CMAKE_CURRENT_SOURCE_DIR})
set(IMBH_C_LIB
    ${IMBH_C_ROOT}/target/release/${CMAKE_SHARED_LIBRARY_PREFIX}imbh_c${CMAKE_SHARED_LIBRARY_SUFFIX})

# Rebuild whenever a Rust source or the manifest changes.
file(GLOB_RECURSE IMBH_C_RUST_SRCS CONFIGURE_DEPENDS ${IMBH_C_ROOT}/src/*.rs)

add_custom_command(
    OUTPUT ${IMBH_C_LIB}
    COMMAND ${CARGO} build --release --manifest-path ${IMBH_C_ROOT}/Cargo.toml
    DEPENDS ${IMBH_C_RUST_SRCS} ${IMBH_C_ROOT}/Cargo.toml ${IMBH_C_ROOT}/build.rs
    COMMENT "Building imbh-c via cargo (release)"
    VERBATIM)
add_custom_target(imbh_c_cargo DEPENDS ${IMBH_C_LIB})

# The shared library is self-contained (all Rust + bundled C deps linked in), so consumers only link
# this one artifact.
add_library(imbh_c SHARED IMPORTED GLOBAL)
add_dependencies(imbh_c imbh_c_cargo)
set_target_properties(imbh_c PROPERTIES
    IMPORTED_LOCATION ${IMBH_C_LIB}
    INTERFACE_INCLUDE_DIRECTORIES ${IMBH_C_ROOT}/include)

option(IMBH_C_BUILD_EXAMPLES "Build the C/C++ quickstart examples" ON)
if(IMBH_C_BUILD_EXAMPLES)
    enable_testing()

    add_executable(quickstart_c examples/c/quickstart.c)
    target_link_libraries(quickstart_c PRIVATE imbh_c)
    add_test(NAME quickstart_c COMMAND quickstart_c)

    add_executable(quickstart_cpp examples/cpp/quickstart.cpp)
    target_link_libraries(quickstart_cpp PRIVATE imbh_c)
    add_test(NAME quickstart_cpp COMMAND quickstart_cpp)

    # Typed-query surface (native C builders).
    add_executable(query_typed_c examples/c/query_typed.c)
    target_link_libraries(query_typed_c PRIVATE imbh_c)
    add_test(NAME query_typed_c COMMAND query_typed_c)
endif()

cmake_minimum_required(VERSION 3.16)
project(imbh_c_test_package LANGUAGES C)

find_package(imbh-c CONFIG REQUIRED)

add_executable(test_imbh_c test_package.c)
target_link_libraries(test_imbh_c PRIVATE imbh-c::imbh-c)

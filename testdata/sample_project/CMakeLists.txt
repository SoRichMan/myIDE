cmake_minimum_required(VERSION 3.16)
project(sample_project CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

add_library(math_utils STATIC src/math_utils.cpp)
target_include_directories(math_utils PUBLIC src)

add_executable(app src/main.cpp)
target_link_libraries(app PRIVATE math_utils)

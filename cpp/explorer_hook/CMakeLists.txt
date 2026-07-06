cmake_minimum_required(VERSION 4.1)
project(explorer_hook)

set(CMAKE_CXX_STANDARD 26)

add_library(explorer_hook STATIC library.cpp)

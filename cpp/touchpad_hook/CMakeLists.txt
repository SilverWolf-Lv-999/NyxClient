cmake_minimum_required(VERSION 4.1)
project(touchpad_hook)

set(CMAKE_CXX_STANDARD 26)

add_library(touchpad_hook STATIC library.cpp)
target_link_libraries(touchpad_hook PUBLIC user32 ole32 oleaut32)

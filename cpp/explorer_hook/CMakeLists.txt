cmake_minimum_required(VERSION 4.1)
project(explorer_hook)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

add_library(explorer_hook SHARED library.cpp)
target_link_libraries(explorer_hook PRIVATE user32)

set(NYX_ASSETS_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../../assets")

add_custom_command(
    TARGET explorer_hook
    POST_BUILD
    COMMAND "${CMAKE_COMMAND}" -E make_directory "${NYX_ASSETS_DIR}"
    COMMAND "${CMAKE_COMMAND}" -E copy_if_different
            "$<TARGET_FILE:explorer_hook>"
            "${NYX_ASSETS_DIR}/$<TARGET_FILE_NAME:explorer_hook>"
)

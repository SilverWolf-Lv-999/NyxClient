#ifndef TOUCHPAD_HOOK_LIBRARY_H
#define TOUCHPAD_HOOK_LIBRARY_H

#ifdef _WIN32

#ifndef __cplusplus
#include <stdbool.h>
#endif

#ifdef __cplusplus
extern "C" {
#endif

bool touchpad_hook_start();
void touchpad_hook_stop();

#ifdef __cplusplus
}
#endif

#endif

#endif // TOUCHPAD_HOOK_LIBRARY_H

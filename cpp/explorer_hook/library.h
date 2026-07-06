#ifndef EXPLORER_HOOK_LIBRARY_H
#define EXPLORER_HOOK_LIBRARY_H

#include <windows.h>

#ifdef __cplusplus
extern "C" {
#endif

__declspec(dllexport) UINT nyx_stage_manager_event_message();
__declspec(dllexport) BOOL nyx_stage_manager_set_event_window(HWND hwnd);
__declspec(dllexport) BOOL nyx_stage_manager_start_hook(HWND hwnd);
__declspec(dllexport) void nyx_stage_manager_stop_hook();

#ifdef __cplusplus
}
#endif

#endif // EXPLORER_HOOK_LIBRARY_H

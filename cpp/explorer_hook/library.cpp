#include "library.h"

#include <atomic>

namespace {
std::atomic<HWND> g_event_window = nullptr;
std::atomic<HWINEVENTHOOK> g_object_hook = nullptr;
std::atomic<HWINEVENTHOOK> g_system_hook = nullptr;

UINT stage_manager_message() {
    static const UINT message = RegisterWindowMessageW(L"NyxClient.StageManager.WinEvent");
    return message;
}

void CALLBACK win_event_proc(
    HWINEVENTHOOK,
    DWORD event,
    HWND hwnd,
    LONG object_id,
    LONG child_id,
    DWORD,
    DWORD
) {
    if (child_id != CHILDID_SELF || (object_id != OBJID_WINDOW && object_id != OBJID_CLIENT)) {
        return;
    }

    const HWND target = g_event_window.load(std::memory_order_acquire);
    if (target == nullptr || !IsWindow(target)) {
        return;
    }

    PostMessageW(target, stage_manager_message(), static_cast<WPARAM>(event), reinterpret_cast<LPARAM>(hwnd));
}
}

UINT nyx_stage_manager_event_message() {
    return stage_manager_message();
}

BOOL nyx_stage_manager_set_event_window(HWND hwnd) {
    if (hwnd != nullptr && !IsWindow(hwnd)) {
        return FALSE;
    }

    g_event_window.store(hwnd, std::memory_order_release);
    return TRUE;
}

BOOL nyx_stage_manager_start_hook(HWND hwnd) {
    if (hwnd != nullptr && !IsWindow(hwnd)) {
        return FALSE;
    }

    nyx_stage_manager_stop_hook();
    g_event_window.store(hwnd, std::memory_order_release);

    const HWINEVENTHOOK object_hook = SetWinEventHook(
        EVENT_OBJECT_CREATE,
        EVENT_OBJECT_UNCLOAKED,
        nullptr,
        win_event_proc,
        0,
        0,
        WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS
    );
    if (object_hook == nullptr) {
        g_event_window.store(nullptr, std::memory_order_release);
        return FALSE;
    }

    const HWINEVENTHOOK system_hook = SetWinEventHook(
        EVENT_SYSTEM_FOREGROUND,
        EVENT_SYSTEM_DESKTOPSWITCH,
        nullptr,
        win_event_proc,
        0,
        0,
        WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS
    );
    if (system_hook == nullptr) {
        UnhookWinEvent(object_hook);
        g_event_window.store(nullptr, std::memory_order_release);
        return FALSE;
    }

    g_object_hook.store(object_hook, std::memory_order_release);
    g_system_hook.store(system_hook, std::memory_order_release);
    return TRUE;
}

void nyx_stage_manager_stop_hook() {
    if (const HWINEVENTHOOK hook = g_object_hook.exchange(nullptr, std::memory_order_acq_rel)) {
        UnhookWinEvent(hook);
    }
    if (const HWINEVENTHOOK hook = g_system_hook.exchange(nullptr, std::memory_order_acq_rel)) {
        UnhookWinEvent(hook);
    }
    g_event_window.store(nullptr, std::memory_order_release);
}

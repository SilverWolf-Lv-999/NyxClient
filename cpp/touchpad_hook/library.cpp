#include "library.h"

#ifdef _WIN32

#define WIN32_LEAN_AND_MEAN
#include <Windows.h>
#include <shldisp.h>

#include <atomic>
#include <cmath>
#include <future>
#include <mutex>
#include <optional>
#include <thread>

namespace {

constexpr UINT kStopMessage = WM_APP + 0x4E60;

struct TapSnapshot {
    POINT point{};
    DWORD time{};
};

std::atomic_bool g_running{false};
std::mutex g_tap_mutex;
std::optional<TapSnapshot> g_last_tap;
std::thread g_hook_thread;
DWORD g_hook_thread_id = 0;

void clear_last_tap() {
    std::lock_guard lock(g_tap_mutex);
    g_last_tap.reset();
}

bool is_desktop_container_class(const wchar_t* class_name) {
    return wcscmp(class_name, L"Progman") == 0 || wcscmp(class_name, L"WorkerW") == 0 ||
           wcscmp(class_name, L"#32769") == 0;
}

bool is_desktop_view_class(const wchar_t* class_name) {
    return wcscmp(class_name, L"SHELLDLL_DefView") == 0 ||
           wcscmp(class_name, L"SysListView32") == 0;
}

bool class_name(HWND hwnd, wchar_t (&buffer)[256]) {
    buffer[0] = L'\0';
    return GetClassNameW(hwnd, buffer, static_cast<int>(std::size(buffer))) > 0;
}

bool is_desktop_window(HWND hwnd) {
    wchar_t name[256]{};
    if (!class_name(hwnd, name)) {
        return false;
    }

    return is_desktop_container_class(name) || is_desktop_view_class(name);
}

bool is_desktop_point(POINT point) {
    HWND hwnd = WindowFromPoint(point);
    if (hwnd == nullptr) {
        return false;
    }
    if (is_desktop_window(hwnd)) {
        return true;
    }

    bool has_desktop_view = false;
    HWND current = hwnd;
    for (int index = 0; index < 16 && current != nullptr; ++index) {
        wchar_t name[256]{};
        if (class_name(current, name)) {
            if (is_desktop_view_class(name)) {
                has_desktop_view = true;
            }
            if (has_desktop_view && is_desktop_container_class(name)) {
                return true;
            }
        }

        current = GetParent(current);
    }

    HWND root = GetAncestor(hwnd, GA_ROOT);
    wchar_t root_name[256]{};
    return has_desktop_view && root != nullptr && class_name(root, root_name) &&
           is_desktop_container_class(root_name);
}

bool is_double_tap(TapSnapshot first, TapSnapshot second) {
    if (second.time - first.time > GetDoubleClickTime()) {
        return false;
    }

    const int max_x = max(GetSystemMetrics(SM_CXDOUBLECLK), 1);
    const int max_y = max(GetSystemMetrics(SM_CYDOUBLECLK), 1);
    return std::abs(second.point.x - first.point.x) <= max_x &&
           std::abs(second.point.y - first.point.y) <= max_y;
}

bool register_tap_and_check_double_tap(POINT point, DWORD time) {
    std::lock_guard lock(g_tap_mutex);

    const TapSnapshot current{point, time};
    const bool matched = g_last_tap.has_value() && is_double_tap(*g_last_tap, current);
    if (matched) {
        g_last_tap.reset();
    } else {
        g_last_tap = current;
    }

    return matched;
}

HRESULT minimize_all_windows() {
    const HRESULT init_result = CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);
    const bool com_initialized = SUCCEEDED(init_result);

    IShellDispatch* shell = nullptr;
    HRESULT result = CoCreateInstance(CLSID_Shell, nullptr, CLSCTX_INPROC_SERVER,
                                      IID_PPV_ARGS(&shell));
    if (SUCCEEDED(result)) {
        result = shell->MinimizeAll();
        shell->Release();
    }

    if (com_initialized) {
        CoUninitialize();
    }

    return result;
}

void spawn_minimize_all_worker() {
    std::thread([] {
        static_cast<void>(minimize_all_windows());
    }).detach();
}

void handle_left_button_up(POINT point, DWORD time) {
    if (!is_desktop_point(point)) {
        clear_last_tap();
        return;
    }

    if (register_tap_and_check_double_tap(point, time)) {
        spawn_minimize_all_worker();
    }
}

LRESULT CALLBACK mouse_hook_proc(int code, WPARAM wparam, LPARAM lparam) {
    if (code == HC_ACTION && wparam == WM_LBUTTONUP) {
        const auto* hook_data = reinterpret_cast<const MSLLHOOKSTRUCT*>(lparam);
        const bool injected =
            (hook_data->flags & LLMHF_INJECTED) != 0 ||
            (hook_data->flags & LLMHF_LOWER_IL_INJECTED) != 0;
        if (!injected) {
            handle_left_button_up(hook_data->pt, hook_data->time);
        }
    }

    return CallNextHookEx(nullptr, code, wparam, lparam);
}

void hook_thread_main(std::promise<bool> startup) {
    g_hook_thread_id = GetCurrentThreadId();

    MSG message{};
    PeekMessageW(&message, nullptr, WM_NULL, WM_NULL, PM_NOREMOVE);

    HMODULE module = GetModuleHandleW(nullptr);
    HHOOK hook = SetWindowsHookExW(WH_MOUSE_LL, mouse_hook_proc, module, 0);
    if (hook == nullptr) {
        startup.set_value(false);
        g_hook_thread_id = 0;
        g_running.store(false, std::memory_order_release);
        return;
    }

    startup.set_value(true);
    while (GetMessageW(&message, nullptr, 0, 0) > 0) {
        if (message.message == kStopMessage || message.message == WM_QUIT) {
            break;
        }

        TranslateMessage(&message);
        DispatchMessageW(&message);
    }

    UnhookWindowsHookEx(hook);
    clear_last_tap();
    g_hook_thread_id = 0;
    g_running.store(false, std::memory_order_release);
}

} // namespace

extern "C" bool touchpad_hook_start() {
    bool expected = false;
    if (!g_running.compare_exchange_strong(expected, true, std::memory_order_acq_rel)) {
        return true;
    }

    std::promise<bool> startup;
    auto result = startup.get_future();
    try {
        g_hook_thread = std::thread(hook_thread_main, std::move(startup));
    } catch (...) {
        g_running.store(false, std::memory_order_release);
        return false;
    }

    if (result.get()) {
        return true;
    }

    if (g_hook_thread.joinable()) {
        g_hook_thread.join();
    }
    return false;
}

extern "C" void touchpad_hook_stop() {
    if (!g_running.load(std::memory_order_acquire)) {
        return;
    }

    const DWORD thread_id = g_hook_thread_id;
    if (thread_id != 0) {
        PostThreadMessageW(thread_id, kStopMessage, 0, 0);
    }

    if (g_hook_thread.joinable()) {
        g_hook_thread.join();
    }

    clear_last_tap();
    g_running.store(false, std::memory_order_release);
}

#endif

/* Tiny x64 bootstrap: only Windows system APIs, no C/VC/UCRT runtime required.
 * Validate the actual package DLL manifest before invoking its renderer probe.
 */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <shellapi.h>

static char manifest[32768];
static WCHAR command[65536];
static WCHAR dll[512];

static DWORD length(const char *s) { DWORD n = 0; while (s[n]) ++n; return n; }
static void write_text(HANDLE f, const char *s) { DWORD written; WriteFile(f, s, length(s), &written, NULL); }
static void write_number(HANDLE f, DWORD value) {
    char digits[16]; DWORD n = 0, written;
    do { digits[n++] = (char)('0' + value % 10); value /= 10; } while (value);
    while (n) WriteFile(f, &digits[--n], 1, &written, NULL);
}
static void append_error(LPCWSTR path, const char *message, DWORD code) {
    HANDLE f = CreateFileW(path, FILE_APPEND_DATA, FILE_SHARE_READ, NULL, OPEN_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    if (f != INVALID_HANDLE_VALUE) { write_text(f, message); write_number(f, code); write_text(f, "\r\n"); CloseHandle(f); }
}

void mainCRTStartup(void) {
    int argc = 0;
    LPWSTR *argv = CommandLineToArgvW(GetCommandLineW(), &argc);
    HANDLE input, output;
    DWORD size = 0, missing = 0;
    SIZE_T pos = 0, at = 0;
    STARTUPINFOW startup;
    PROCESS_INFORMATION process;
    SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);
    if (!argv || argc != 4) ExitProcess(64);
    output = CreateFileW(argv[3], GENERIC_WRITE, FILE_SHARE_READ, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    if (output == INVALID_HANDLE_VALUE) ExitProcess(74);
    input = CreateFileW(argv[2], GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, NULL);
    if (input == INVALID_HANDLE_VALUE || !ReadFile(input, manifest, sizeof(manifest)-1, &size, NULL) || !size || size >= sizeof(manifest)-1) {
        write_text(output, "Invalid or missing package dependency manifest.\r\n"); CloseHandle(output); ExitProcess(65);
    }
    CloseHandle(input);
    manifest[size] = 0;
    while (pos < size) {
        DWORD n = 0;
        char name[512];
        HMODULE library;
        while (pos < size && (manifest[pos] == '\r' || manifest[pos] == '\n')) ++pos;
        if (pos >= size) break;
        while (pos < size && manifest[pos] != '\r' && manifest[pos] != '\n') {
            char c = manifest[pos++];
            if (n >= 510 || !((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '-' || c == '_' || c == '.')) {
                write_text(output, "Invalid DLL name in package manifest.\r\n"); CloseHandle(output); ExitProcess(65);
            }
            name[n] = c; dll[n] = (WCHAR)c; ++n;
        }
        name[n] = 0; dll[n] = 0;
        /* API-set contracts must be loaded, not checked as physical DLL files. */
        library = LoadLibraryExW(dll, NULL, LOAD_LIBRARY_SEARCH_SYSTEM32);
        if (!library) {
            DWORD error = GetLastError();
            write_text(output, "Missing/unusable x64 DLL: "); write_text(output, name);
            write_text(output, " (Windows error "); write_number(output, error); write_text(output, ")\r\n"); ++missing;
        } else { FreeLibrary(library); }
    }
    CloseHandle(output);
    if (missing) ExitProcess(20);
    /* Build a quoted command line; Windows file names cannot contain a quote. */
    command[at++] = L'"';
    for (pos = 0; argv[1][pos] && at < 32000; ++pos) command[at++] = argv[1][pos];
    command[at++] = L'"';
    { LPCWSTR flag = L" --check-install \""; for (pos = 0; flag[pos]; ++pos) command[at++] = flag[pos]; }
    for (pos = 0; argv[3][pos] && at < 65000; ++pos) command[at++] = argv[3][pos];
    command[at++] = L'"'; command[at] = 0;
    { BYTE *p = (BYTE*)&startup; for (pos = 0; pos < sizeof(startup); ++pos) p[pos] = 0; }
    { BYTE *p = (BYTE*)&process; for (pos = 0; pos < sizeof(process); ++pos) p[pos] = 0; }
    startup.cb = sizeof(startup);
    startup.dwFlags = STARTF_USESHOWWINDOW; startup.wShowWindow = SW_HIDE;
    if (!CreateProcessW(argv[1], command, NULL, NULL, FALSE, CREATE_NO_WINDOW, NULL, NULL, &startup, &process)) {
        append_error(argv[3], "Could not start renderer check. Windows error: ", GetLastError()); ExitProcess(22);
    }
    CloseHandle(process.hThread);
    if (WaitForSingleObject(process.hProcess, 60000) != WAIT_OBJECT_0) {
        TerminateProcess(process.hProcess, 23); WaitForSingleObject(process.hProcess, 5000); CloseHandle(process.hProcess);
        append_error(argv[3], "Renderer check timed out. Update graphics drivers. Code: ", 23); ExitProcess(23);
    }
    { DWORD code = 22; GetExitCodeProcess(process.hProcess, &code); CloseHandle(process.hProcess);
      if (code) append_error(argv[3], "Renderer process exit code: ", code);
      LocalFree(argv); ExitProcess(code ? 21 : 0); }
}

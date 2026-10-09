// GPU runner fault fixture. Compile three copies named allocation, crash, timeout.
#include <windows.h>
#include <stdio.h>
#include <wchar.h>
int main(void) {
    wchar_t name[32768]; GetModuleFileNameW(NULL, name, 32768);
    puts("DISCARD THIS"); fflush(stdout);
    if (wcsstr(name, L"timeout")) Sleep(15000);
    if (wcsstr(name, L"allocation")) fputs("CUDA allocation failed: out of memory\n", stderr);
    return 17;
}

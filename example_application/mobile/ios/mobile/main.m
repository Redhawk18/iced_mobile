//
//  main.m
//  mobile
//
//  Minimal trampoline that calls into the Rust static library.
//  winit handles the entire iOS app lifecycle (UIWindow, UIViewController,
//  event loop) internally when start_app() calls EventLoop::run_app().
//  Do NOT call UIApplicationMain here — winit does that.
//

// Declare the Rust entry point (exported from libmobile.a)
extern void start_app(void);

int main(int argc, char* argv[]) {
    // This never returns on iOS — winit takes over the run loop
    start_app();
    return 0;
}

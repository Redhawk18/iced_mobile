# Iced Mobile library

This is a test library that makes possible to run Iced on iOS and Android. From usage perspective, it only requires calling `mobile_run` instead of `run` on the application and adding `use iced_mobile::MobileAppRunner;`.

The best approach at the moment is to have a separate crate for mobile versions of the application and use the regular application crate as a dependency. 

The mobile crate can be stored in the same repository. I've created a example in the `example_application` directory. Android can be build using the `cargo-apk` utility. 
For iOS there is example xcode project in the `example_application` directory.

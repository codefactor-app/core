//! Native-call recognition is supplied by the registered language strategy.
use crate::LanguageStrategy;

pub fn function(strategy: &dyn LanguageStrategy, symbol: &str) -> bool {
    strategy.is_native_function(symbol)
}
pub fn receiver(strategy: &dyn LanguageStrategy, symbol: &str) -> bool {
    strategy.is_native_receiver(symbol)
}
pub fn global_receiver(strategy: &dyn LanguageStrategy, symbol: &str) -> bool {
    strategy.is_native_global_receiver(symbol)
}
pub fn method(strategy: &dyn LanguageStrategy, owner: &str, name: &str) -> bool {
    strategy.is_native_method(owner, name)
}
pub fn macro_name(strategy: &dyn LanguageStrategy, name: &str) -> bool {
    strategy.is_native_macro(name)
}

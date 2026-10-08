//! Shared query construction, with an adapter that can record the actual builder calls.

use super::SERVICE_NAME;

#[derive(Clone, Copy)]
pub(super) enum Purpose<'a> {
    Exists(&'a str),
    List,
    Delete(&'a str),
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum OptionValue<'a> {
    GenericPassword,
    Service(&'a str),
    Account(&'a str),
    CaseInsensitive(bool),
    Attributes(bool),
    Data(bool),
    References(bool),
    All,
}

pub(super) trait Builder<K> {
    fn keychain(&mut self, keychain: &K);
    fn option(&mut self, option: OptionValue<'_>);
}

pub(super) fn configure<K>(builder: &mut impl Builder<K>, keychain: &K, purpose: Purpose<'_>) {
    builder.keychain(keychain);
    builder.option(OptionValue::GenericPassword);
    builder.option(OptionValue::Service(SERVICE_NAME));
    builder.option(OptionValue::CaseInsensitive(false));
    if let Purpose::Exists(account) | Purpose::Delete(account) = purpose {
        builder.option(OptionValue::Account(account));
    }
    builder.option(OptionValue::Data(false));
    builder.option(OptionValue::References(false));
    match purpose {
        Purpose::Exists(_) => builder.option(OptionValue::Attributes(true)),
        Purpose::List => {
            builder.option(OptionValue::Attributes(true));
            builder.option(OptionValue::All);
        }
        Purpose::Delete(_) => builder.option(OptionValue::Attributes(false)),
    }
}

#[cfg(target_os = "macos")]
impl Builder<security_framework::os::macos::keychain::SecKeychain>
    for security_framework::item::ItemSearchOptions
{
    fn keychain(&mut self, keychain: &security_framework::os::macos::keychain::SecKeychain) {
        self.keychains(std::slice::from_ref(keychain));
    }

    fn option(&mut self, option: OptionValue<'_>) {
        use security_framework::item::{ItemClass, Limit};

        match option {
            OptionValue::GenericPassword => {
                self.class(ItemClass::generic_password());
            }
            OptionValue::Service(service) => {
                self.service(service);
            }
            OptionValue::Account(account) => {
                self.account(account);
            }
            OptionValue::CaseInsensitive(value) => {
                self.case_insensitive(Some(value));
            }
            OptionValue::Attributes(value) => {
                self.load_attributes(value);
            }
            OptionValue::Data(value) => {
                self.load_data(value);
            }
            OptionValue::References(value) => {
                self.load_refs(value);
            }
            OptionValue::All => {
                self.limit(Limit::All);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Spy {
        keychain: Option<&'static str>,
        options: Vec<String>,
    }

    impl Builder<&'static str> for Spy {
        fn keychain(&mut self, keychain: &&'static str) {
            self.keychain = Some(*keychain);
        }

        fn option(&mut self, option: OptionValue<'_>) {
            self.options.push(format!("{option:?}"));
        }
    }

    fn has(spy: &Spy, option: OptionValue<'_>) -> bool {
        spy.options.contains(&format!("{option:?}"))
    }

    fn assert_namespace_and_no_secret_data(spy: &Spy) {
        assert_eq!(spy.keychain, Some("selected-login-keychain"));
        assert!(has(spy, OptionValue::GenericPassword));
        assert!(has(spy, OptionValue::Service("dev.kagitaba.kagitaba")));
        assert!(has(spy, OptionValue::CaseInsensitive(false)));
        assert!(has(spy, OptionValue::Data(false)));
        assert!(has(spy, OptionValue::References(false)));
        assert!(!has(spy, OptionValue::Data(true)));
    }

    #[test]
    fn existence_query_is_scoped_to_exact_account_and_attributes_only() {
        let mut spy = Spy::default();
        configure(
            &mut spy,
            &"selected-login-keychain",
            Purpose::Exists("API_KEY"),
        );
        assert_namespace_and_no_secret_data(&spy);
        assert!(has(&spy, OptionValue::Account("API_KEY")));
        assert!(has(&spy, OptionValue::Attributes(true)));
        assert!(!has(&spy, OptionValue::All));
    }

    #[test]
    fn listing_requests_all_attributes_in_the_selected_namespace() {
        let mut spy = Spy::default();
        configure(&mut spy, &"selected-login-keychain", Purpose::List);
        assert_namespace_and_no_secret_data(&spy);
        assert!(has(&spy, OptionValue::Attributes(true)));
        assert!(has(&spy, OptionValue::All));
        assert!(
            !spy.options
                .iter()
                .any(|value| value.starts_with("Account("))
        );
    }

    #[test]
    fn deletion_is_scoped_to_exact_account_without_return_flags_or_all_limit() {
        let mut spy = Spy::default();
        configure(
            &mut spy,
            &"selected-login-keychain",
            Purpose::Delete("API_KEY"),
        );
        assert_namespace_and_no_secret_data(&spy);
        assert!(has(&spy, OptionValue::Account("API_KEY")));
        assert!(has(&spy, OptionValue::Attributes(false)));
        assert!(!has(&spy, OptionValue::Attributes(true)));
        assert!(!has(&spy, OptionValue::All));
    }
}

use crate::profile::Profile;
use crate::types::ProfileId;

/// App-global registry of available shell profiles.
///
/// Stored as `Entity<ProfileRegistry>` — shared across all windows.
/// Created once at app startup from detected shells.
pub struct ProfileRegistry {
    profiles: Vec<Profile>,
    default_profile_id: ProfileId,
}

impl ProfileRegistry {
    pub fn new(profiles: Vec<Profile>, default_profile_id: ProfileId) -> Self {
        assert!(
            !profiles.is_empty(),
            "ProfileRegistry must have at least one profile"
        );
        assert!(
            profiles.iter().any(|p| p.id == default_profile_id),
            "default_profile_id must reference an existing profile"
        );
        Self {
            profiles,
            default_profile_id,
        }
    }

    /// All available profiles.
    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    /// The default profile (used for new tabs).
    pub fn default_profile(&self) -> &Profile {
        self.profile_by_id(self.default_profile_id)
            .expect("default profile missing from registry")
    }

    /// Look up a profile by ID.
    pub fn profile_by_id(&self, id: ProfileId) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    /// The default profile's ID.
    pub fn default_profile_id(&self) -> ProfileId {
        self.default_profile_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::ShellKind;

    fn test_profiles() -> (Vec<Profile>, ProfileId) {
        let p1 = Profile::new("cmd".into(), ShellKind::CommandPrompt);
        let p2 = Profile::new("PS".into(), ShellKind::PowerShell);
        let default_id = p1.id;
        (vec![p1, p2], default_id)
    }

    #[test]
    fn registry_creation() {
        let (profiles, default_id) = test_profiles();
        let registry = ProfileRegistry::new(profiles, default_id);
        assert_eq!(registry.profiles().len(), 2);
        assert_eq!(registry.default_profile().id, default_id);
    }

    #[test]
    fn profile_by_id_found() {
        let (profiles, default_id) = test_profiles();
        let registry = ProfileRegistry::new(profiles, default_id);
        assert!(registry.profile_by_id(default_id).is_some());
    }

    #[test]
    fn profile_by_id_not_found() {
        let (profiles, default_id) = test_profiles();
        let registry = ProfileRegistry::new(profiles, default_id);
        let fake_id = ProfileId::new();
        assert!(registry.profile_by_id(fake_id).is_none());
    }
}

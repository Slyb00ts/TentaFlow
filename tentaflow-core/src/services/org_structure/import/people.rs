//! Who a login or an e-mail in the file is. Members of the organization first
//! (by login, by e-mail, by full name), then its people without an account.

use std::collections::HashMap;

use rusqlite::Connection;

use crate::services::org_structure::error::Result;
use crate::services::org_structure::types::Subject;

pub struct Member {
    pub id: String,
    pub login: String,
    pub email: String,
    /// The display name, or the login when there is none — as the structure shows it.
    pub name: String,
    /// The display name as stored: empty when the account has none. A file
    /// meant for a member who may not see logins must not fall back to one.
    pub display: String,
}

pub struct External {
    pub id: String,
    pub name: String,
    pub email: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    Found(Subject),
    /// Several accounts answer to the text (a shared name); the logins.
    Ambiguous(Vec<String>),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub login: String,
    pub name: String,
}

pub struct Directory {
    members: Vec<Member>,
    externals: Vec<External>,
    by_login: HashMap<String, usize>,
    by_email: HashMap<String, Vec<usize>>,
    by_name: HashMap<String, Vec<usize>>,
    external_by_email: HashMap<String, Vec<usize>>,
    external_by_name: HashMap<String, Vec<usize>>,
}

fn key(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The words of a name in alphabetical order: "Kowalski Jan" and "Jan
/// Kowalski" are then the same spelling.
fn sorted_words(text: &str) -> String {
    let mut words: Vec<&str> = text.split(' ').collect();
    words.sort_unstable();
    words.join(" ")
}

fn push_key(map: &mut HashMap<String, Vec<usize>>, text: &str, index: usize) {
    let key = key(text);
    if !key.is_empty() {
        map.entry(key).or_default().push(index);
    }
}

impl Directory {
    pub fn load(conn: &Connection, org_id: &str) -> Result<Self> {
        let mut members = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT u.id, u.username, COALESCE(u.email, ''), \
                    COALESCE(NULLIF(u.display_name, ''), u.username), \
                    COALESCE(u.display_name, '') \
             FROM user_accounts u JOIN org_memberships m ON m.user_id = u.id \
             WHERE m.org_id = ?1 ORDER BY u.username",
        )?;
        for row in stmt.query_map([org_id], |r| {
            Ok(Member {
                id: r.get(0)?,
                login: r.get(1)?,
                email: r.get(2)?,
                name: r.get(3)?,
                display: r.get(4)?,
            })
        })? {
            members.push(row?);
        }
        let mut externals = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT id, display_name, COALESCE(email, '') FROM org_external_persons \
             WHERE org_id = ?1 ORDER BY display_name, id",
        )?;
        for row in stmt.query_map([org_id], |r| {
            Ok(External {
                id: r.get(0)?,
                name: r.get(1)?,
                email: r.get(2)?,
            })
        })? {
            externals.push(row?);
        }
        Ok(Self::new(members, externals))
    }

    pub fn new(members: Vec<Member>, externals: Vec<External>) -> Self {
        let mut by_login = HashMap::new();
        let mut by_email = HashMap::new();
        let mut by_name = HashMap::new();
        for (index, member) in members.iter().enumerate() {
            by_login.entry(key(&member.login)).or_insert(index);
            push_key(&mut by_email, &member.email, index);
            push_key(&mut by_name, &member.name, index);
        }
        let mut external_by_email = HashMap::new();
        let mut external_by_name = HashMap::new();
        for (index, external) in externals.iter().enumerate() {
            push_key(&mut external_by_email, &external.email, index);
            push_key(&mut external_by_name, &external.name, index);
        }
        Self {
            members,
            externals,
            by_login,
            by_email,
            by_name,
            external_by_email,
            external_by_name,
        }
    }

    pub fn member(&self, user_id: &str) -> Option<&Member> {
        self.members.iter().find(|m| m.id == user_id)
    }

    /// Who a holder is, for a report: the name the structure shows.
    pub fn label(&self, subject: &Subject) -> String {
        match subject {
            Subject::User(id) => self.member(id).map(|m| m.name.clone()),
            Subject::External(id) => self.external(id).map(|e| e.name.clone()),
        }
        .unwrap_or_default()
    }

    pub fn external(&self, external_id: &str) -> Option<&External> {
        self.externals.iter().find(|e| e.id == external_id)
    }

    /// Login first, then e-mail, then the full name; an account beats a
    /// person without one. A text that fits several people of one kind is
    /// ambiguous — guessing who a "Jan Kowalski" is would put the wrong
    /// person on a position.
    pub fn resolve(&self, cell: &str) -> Lookup {
        let key = key(cell);
        if let Some(index) = self.by_login.get(&key) {
            return Lookup::Found(Subject::User(self.members[*index].id.clone()));
        }
        for map in [&self.by_email, &self.by_name] {
            match map.get(&key).map(Vec::as_slice) {
                Some([one]) => return Lookup::Found(Subject::User(self.members[*one].id.clone())),
                Some(many) => {
                    return Lookup::Ambiguous(
                        many.iter()
                            .map(|index| self.members[*index].login.clone())
                            .collect(),
                    )
                }
                None => {}
            }
        }
        for map in [&self.external_by_email, &self.external_by_name] {
            match map.get(&key).map(Vec::as_slice) {
                Some([one]) => {
                    return Lookup::Found(Subject::External(self.externals[*one].id.clone()))
                }
                Some(many) => {
                    return Lookup::Ambiguous(
                        many.iter()
                            .map(|index| self.externals[*index].name.clone())
                            .collect(),
                    )
                }
                None => {}
            }
        }
        Lookup::Unknown
    }

    /// The member the text is most likely a typo of. Only a member with ONE
    /// clearly closest spelling is suggested: two equally close accounts mean
    /// no suggestion, not a coin toss.
    pub fn suggest(&self, cell: &str) -> Option<Suggestion> {
        let typed = key(cell);
        let mut queries = vec![typed.chars().collect::<Vec<_>>()];
        if let Some((local, _)) = typed.split_once('@') {
            queries.push(local.chars().collect());
        }
        queries.push(sorted_words(&typed).chars().collect());
        let longest = queries.iter().map(Vec::len).max().unwrap_or(0);
        let allowed = match longest {
            0..=3 => 0,
            4..=7 => 1,
            _ => 2,
        };
        if allowed == 0 {
            return None;
        }
        let mut best: Option<(usize, usize)> = None;
        let mut tied = false;
        for (index, member) in self.members.iter().enumerate() {
            let distance = self
                .spellings(member)
                .iter()
                .flat_map(|spelling| {
                    queries
                        .iter()
                        .map(move |query| edit_distance(query, spelling))
                })
                .min()
                .unwrap_or(usize::MAX);
            if distance > allowed {
                continue;
            }
            match best {
                Some((_, d)) if distance > d => {}
                Some((_, d)) if distance == d => tied = true,
                _ => {
                    best = Some((index, distance));
                    tied = false;
                }
            }
        }
        match best {
            Some((index, _)) if !tied => Some(Suggestion {
                login: self.members[index].login.clone(),
                name: self.members[index].name.clone(),
            }),
            _ => None,
        }
    }

    /// The ways a member is written: login, e-mail, its local part, the full
    /// name and the name with its words in any order ("Kowalski Jan").
    fn spellings(&self, member: &Member) -> Vec<Vec<char>> {
        let mut spellings = vec![key(&member.login), key(&member.email), key(&member.name)];
        if let Some((local, _)) = key(&member.email).split_once('@') {
            spellings.push(local.to_string());
        }
        spellings.push(sorted_words(&key(&member.name)));
        spellings
            .into_iter()
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().collect())
            .collect()
    }
}

/// The closest name to a typo, by the same rule as `suggest`: one clearly
/// closest spelling or nothing.
pub fn closest_name<'a>(typed: &str, names: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let typed: Vec<char> = key(typed).chars().collect();
    let allowed = match typed.len() {
        0..=3 => 0,
        4..=7 => 1,
        _ => 2,
    };
    let mut best: Option<(usize, &str)> = None;
    let mut tied = false;
    for name in names {
        let distance = edit_distance(&typed, &key(name).chars().collect::<Vec<_>>());
        if distance > allowed {
            continue;
        }
        match best {
            Some((d, _)) if distance > d => {}
            Some((d, _)) if distance == d => tied = true,
            _ => {
                best = Some((distance, name));
                tied = false;
            }
        }
    }
    best.filter(|_| !tied).map(|(_, name)| name.to_string())
}

/// Optimal string alignment distance: insertions, deletions, substitutions
/// and the swap of two neighbours — the typos of a hand typing a login.
fn edit_distance(a: &[char], b: &[char]) -> usize {
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[n][m]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: &str, login: &str, email: &str, name: &str) -> Member {
        Member {
            id: id.into(),
            login: login.into(),
            email: email.into(),
            name: name.into(),
            display: name.into(),
        }
    }

    fn directory() -> Directory {
        Directory::new(
            vec![
                member("u1", "j.kowalski", "jan.kowalski@firma.pl", "Jan Kowalski"),
                member("u2", "a.nowak", "anna.nowak@firma.pl", "Anna Nowak"),
                member("u3", "m.nowak", "marek.nowak@firma.pl", "Nowak"),
                member("u4", "k.wisniewski", "k1@firma.pl", "Ewa Nowak"),
            ],
            vec![External {
                id: "x1".into(),
                name: "Piotr Zewnętrzny".into(),
                email: "piotr@partner.pl".into(),
            }],
        )
    }

    #[test]
    fn a_login_an_email_and_a_full_name_all_find_the_member() {
        let dir = directory();
        for cell in [
            "j.kowalski",
            "J.Kowalski",
            " jan.kowalski@firma.pl ",
            "Jan  Kowalski",
        ] {
            assert_eq!(
                dir.resolve(cell),
                Lookup::Found(Subject::User("u1".into())),
                "{cell}"
            );
        }
    }

    #[test]
    fn a_person_without_an_account_is_found_by_email_or_name() {
        let dir = directory();
        for cell in ["piotr@partner.pl", "Piotr Zewnętrzny"] {
            assert_eq!(
                dir.resolve(cell),
                Lookup::Found(Subject::External("x1".into()))
            );
        }
    }

    #[test]
    fn a_name_two_accounts_share_is_ambiguous_not_a_guess() {
        let dir = Directory::new(
            vec![
                member("u1", "a1", "a1@x.pl", "Jan Kowalski"),
                member("u2", "a2", "a2@x.pl", "Jan Kowalski"),
            ],
            vec![],
        );
        assert_eq!(
            dir.resolve("Jan Kowalski"),
            Lookup::Ambiguous(vec!["a1".into(), "a2".into()])
        );
    }

    #[test]
    fn a_typo_in_a_login_gets_the_closest_member() {
        let dir = directory();
        assert_eq!(dir.resolve("j.kowlski"), Lookup::Unknown);
        assert_eq!(
            dir.suggest("j.kowlski"),
            Some(Suggestion {
                login: "j.kowalski".into(),
                name: "Jan Kowalski".into()
            })
        );
        // A swapped pair of letters and words in another order.
        assert_eq!(
            dir.suggest("j.kowlaski").map(|s| s.login),
            Some("j.kowalski".into())
        );
        assert_eq!(
            dir.suggest("Kowalski Jan").map(|s| s.login),
            Some("j.kowalski".into())
        );
    }

    #[test]
    fn two_equally_close_members_or_nobody_close_means_no_suggestion() {
        let dir = directory();
        // One edit from both a.nowak and m.nowak.
        assert_eq!(dir.suggest("x.nowak"), None);
        assert_eq!(dir.suggest("zupelnie.kto.inny"), None);
        assert_eq!(dir.suggest("ab"), None);
    }

    #[test]
    fn the_closest_name_of_a_unit_type() {
        let names = ["Dział", "Zespół", "Pion"];
        assert_eq!(closest_name("Dzial", names), Some("Dział".to_string()));
        assert_eq!(closest_name("Oddział", names), None);
    }
}

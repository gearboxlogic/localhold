use std::collections::BTreeMap;

use super::Predicate;

#[derive(Clone, Copy)]
struct Literal {
    variable: usize,
    positive: bool,
}

enum ClauseState {
    Satisfied,
    Conflict,
    Unit(Literal),
    Unresolved,
}

#[derive(Default)]
struct Encoder {
    atoms: BTreeMap<String, usize>,
    exclusive_groups: BTreeMap<String, Vec<usize>>,
    clauses: Vec<Vec<Literal>>,
    variable_count: usize,
}

pub(super) fn is_satisfiable(predicate: &Predicate) -> bool {
    let mut encoder = Encoder::default();
    let root = encoder.encode(predicate);
    encoder.clauses.push(vec![Literal { variable: root, positive: true }]);
    solve(&encoder.clauses, vec![None; encoder.variable_count])
}

impl Encoder {
    fn encode(&mut self, predicate: &Predicate) -> usize {
        match predicate {
            Predicate::Constant(value) => {
                let variable = self.new_variable();
                self.clauses.push(vec![Literal { variable, positive: *value }]);
                variable
            }
            Predicate::Atom {
                identity,
                exclusive_group,
                required_target_families,
            } => {
                let variable = self.atom(identity, exclusive_group.as_deref());
                for family in required_target_families {
                    let required = self.atom(&super::target_family_identity(family), None);
                    self.clauses.push(vec![
                        Literal { variable, positive: false },
                        Literal {
                            variable: required,
                            positive: true,
                        },
                    ]);
                }
                variable
            }
            Predicate::All(nested) => {
                let children = nested.iter().map(|predicate| self.encode(predicate)).collect::<Vec<_>>();
                self.encode_all(&children)
            }
            Predicate::Any(nested) => {
                let children = nested.iter().map(|predicate| self.encode(predicate)).collect::<Vec<_>>();
                self.encode_any(&children)
            }
            Predicate::Not(nested) => {
                let child = self.encode(nested);
                let variable = self.new_variable();
                self.clauses.push(vec![Literal { variable, positive: false }, Literal { variable: child, positive: false }]);
                self.clauses.push(vec![Literal { variable, positive: true }, Literal { variable: child, positive: true }]);
                variable
            }
        }
    }

    fn encode_all(&mut self, children: &[usize]) -> usize {
        let variable = self.new_variable();
        for child in children {
            self.clauses.push(vec![Literal { variable, positive: false }, Literal { variable: *child, positive: true }]);
        }
        let mut clause = Vec::with_capacity(children.len().saturating_add(1));
        clause.push(Literal { variable, positive: true });
        clause.extend(children.iter().map(|child| Literal {
            variable: *child,
            positive: false,
        }));
        self.clauses.push(clause);
        variable
    }

    fn encode_any(&mut self, children: &[usize]) -> usize {
        let variable = self.new_variable();
        for child in children {
            self.clauses.push(vec![
                Literal {
                    variable: *child,
                    positive: false,
                },
                Literal { variable, positive: true },
            ]);
        }
        let mut clause = Vec::with_capacity(children.len().saturating_add(1));
        clause.push(Literal { variable, positive: false });
        clause.extend(children.iter().map(|child| Literal { variable: *child, positive: true }));
        self.clauses.push(clause);
        variable
    }

    const fn new_variable(&mut self) -> usize {
        let variable = self.variable_count;
        self.variable_count = match self.variable_count.checked_add(1) {
            Some(next) => next,
            None => panic!("cfg predicate variable count overflow"),
        };
        variable
    }

    fn atom(&mut self, identity: &str, exclusive_group: Option<&str>) -> usize {
        let variable = if let Some(variable) = self.atoms.get(identity) {
            *variable
        } else {
            let variable = self.new_variable();
            self.atoms.insert(identity.to_owned(), variable);
            variable
        };
        if let Some(group) = exclusive_group {
            let peers = self.exclusive_groups.entry(group.to_owned()).or_default();
            if !peers.contains(&variable) {
                self.clauses.extend(
                    peers
                        .iter()
                        .map(|peer| vec![Literal { variable, positive: false }, Literal { variable: *peer, positive: false }]),
                );
                peers.push(variable);
            }
        }
        self.restrict_incompatible_target_families(identity, variable);
        variable
    }

    fn restrict_incompatible_target_families(&mut self, identity: &str, variable: usize) {
        let incompatible = match identity {
            "target-family:4:unix" => "target-family:7:windows",
            "target-family:7:windows" => "target-family:4:unix",
            _ => return,
        };
        if let Some(peer) = self.atoms.get(incompatible) {
            self.clauses.push(vec![Literal { variable, positive: false }, Literal { variable: *peer, positive: false }]);
        }
    }
}

fn solve(clauses: &[Vec<Literal>], mut assignment: Vec<Option<bool>>) -> bool {
    if !propagate_units(clauses, &mut assignment) {
        return false;
    }
    let Some(variable) = unresolved_variable(clauses, &assignment) else {
        return true;
    };
    [false, true].into_iter().any(|value| {
        let mut branch = assignment.clone();
        branch[variable] = Some(value);
        solve(clauses, branch)
    })
}

fn propagate_units(clauses: &[Vec<Literal>], assignment: &mut [Option<bool>]) -> bool {
    loop {
        let mut unit = None;
        for clause in clauses {
            match clause_state(clause, assignment) {
                ClauseState::Satisfied | ClauseState::Unresolved => {}
                ClauseState::Conflict => return false,
                ClauseState::Unit(literal) => {
                    unit = Some(literal);
                    break;
                }
            }
        }
        let Some(unit) = unit else {
            return true;
        };
        match assignment[unit.variable] {
            Some(value) if value != unit.positive => return false,
            Some(_) => {}
            None => assignment[unit.variable] = Some(unit.positive),
        }
    }
}

fn clause_state(clause: &[Literal], assignment: &[Option<bool>]) -> ClauseState {
    if clause.iter().any(|literal| assignment[literal.variable].is_some_and(|value| value == literal.positive)) {
        return ClauseState::Satisfied;
    }
    let mut unresolved = clause.iter().filter(|literal| assignment[literal.variable].is_none());
    match (unresolved.next(), unresolved.next()) {
        (None, _) => ClauseState::Conflict,
        (Some(literal), None) => ClauseState::Unit(*literal),
        (Some(_), Some(_)) => ClauseState::Unresolved,
    }
}

fn unresolved_variable(clauses: &[Vec<Literal>], assignment: &[Option<bool>]) -> Option<usize> {
    clauses
        .iter()
        .filter(|clause| !clause.iter().any(|literal| assignment[literal.variable].is_some_and(|value| value == literal.positive)))
        .flat_map(|clause| clause.iter())
        .find_map(|literal| assignment[literal.variable].is_none().then_some(literal.variable))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contradiction_remains_unsatisfiable_beyond_enumeration_sized_inputs() {
        let atom = |identity: String| Predicate::Atom {
            identity,
            exclusive_group: None,
            required_target_families: Vec::new(),
        };
        let mut predicates = (0..24).map(|index| atom(format!("feature-{index}"))).collect::<Vec<_>>();
        predicates.push(atom("contradiction".to_owned()));
        predicates.push(Predicate::Not(Box::new(atom("contradiction".to_owned()))));
        assert!(!is_satisfiable(&Predicate::All(predicates)));
    }

    #[test]
    fn large_disjunction_with_one_available_branch_is_satisfiable() {
        let alternatives = (0..24)
            .map(|index| Predicate::Atom {
                identity: format!("feature-{index}"),
                exclusive_group: None,
                required_target_families: Vec::new(),
            })
            .collect::<Vec<_>>();
        assert!(is_satisfiable(&Predicate::Any(alternatives)));
    }
}

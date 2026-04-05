#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operator {
    Delete,
    Change,
    Yank,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindType {
    F,
    FBackward,
    T,
    TBackward,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextObjScope {
    Inner,
    Around,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandState {
    Idle,
    Count {
        digits: String,
    },
    Operator {
        op: Operator,
        count: usize,
    },
    OperatorCount {
        op: Operator,
        count: usize,
        digits: String,
    },
    OperatorFind {
        op: Operator,
        count: usize,
        find: FindType,
    },
    OperatorTextObj {
        op: Operator,
        count: usize,
        scope: TextObjScope,
    },
    Find {
        find: FindType,
        count: usize,
    },
    G {
        count: usize,
    },
    OperatorG {
        op: Operator,
        count: usize,
    },
    Replace {
        count: usize,
    },
    Indent {
        dir: char,
        count: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum VimMode {
    #[default]
    Insert,
    Normal(CommandState),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VimState {
    pub enabled: bool,
    pub mode: VimMode,
}

impl Default for VimState {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: VimMode::Insert,
        }
    }
}

impl VimState {
    pub fn is_insert(&self) -> bool {
        !self.enabled || matches!(self.mode, VimMode::Insert)
    }

    pub fn enter_normal(&mut self) {
        self.mode = VimMode::Normal(CommandState::Idle);
    }

    pub fn enter_insert(&mut self) {
        self.mode = VimMode::Insert;
    }
}

pub enum VimTransition {
    None,
    EnterInsert,
    MoveCursor(isize),
    SetCursor(usize),
    DeleteChars(usize),
    ReplaceChar(char),
    // Expand this as needed
}

pub fn handle_normal_key(state: &mut CommandState, key: char) -> VimTransition {
    // Basic subset of vim keys
    match state {
        CommandState::Idle => {
            match key {
                'i' => VimTransition::EnterInsert,
                'a' => VimTransition::MoveCursor(1), // usually enters insert after moving, handled specially for now
                'h' => VimTransition::MoveCursor(-1),
                'l' => VimTransition::MoveCursor(1),
                'x' => VimTransition::DeleteChars(1),
                '0' => VimTransition::SetCursor(0),
                '1'..='9' => {
                    *state = CommandState::Count {
                        digits: key.to_string(),
                    };
                    VimTransition::None
                }
                'd' => {
                    *state = CommandState::Operator {
                        op: Operator::Delete,
                        count: 1,
                    };
                    VimTransition::None
                }
                'c' => {
                    *state = CommandState::Operator {
                        op: Operator::Change,
                        count: 1,
                    };
                    VimTransition::None
                }
                _ => VimTransition::None,
            }
        }
        CommandState::Count { digits } => {
            if key.is_ascii_digit() {
                digits.push(key);
                VimTransition::None
            } else {
                let count = digits.parse::<usize>().unwrap_or(1);
                // process key with count
                match key {
                    'h' => {
                        *state = CommandState::Idle;
                        VimTransition::MoveCursor(-(count as isize))
                    }
                    'l' => {
                        *state = CommandState::Idle;
                        VimTransition::MoveCursor(count as isize)
                    }
                    'x' => {
                        *state = CommandState::Idle;
                        VimTransition::DeleteChars(count)
                    }
                    'd' => {
                        *state = CommandState::Operator {
                            op: Operator::Delete,
                            count,
                        };
                        VimTransition::None
                    }
                    'c' => {
                        *state = CommandState::Operator {
                            op: Operator::Change,
                            count,
                        };
                        VimTransition::None
                    }
                    _ => {
                        *state = CommandState::Idle;
                        VimTransition::None
                    }
                }
            }
        }
        CommandState::Operator { op, count } => {
            let _count = *count;
            // very basic
            if *op == Operator::Delete && key == 'd' {
                *state = CommandState::Idle;
                // delete line (simplified)
                VimTransition::DeleteChars(9999)
            } else if *op == Operator::Change && key == 'c' {
                *state = CommandState::Idle;
                // change line -> simplify to delete
                VimTransition::EnterInsert
            } else {
                *state = CommandState::Idle;
                VimTransition::None
            }
        }
        _ => {
            *state = CommandState::Idle;
            VimTransition::None
        }
    }
}

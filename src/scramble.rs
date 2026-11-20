//! Random-move scramble generation for 3x3.

use rand::Rng;
use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Face {
    U,
    D,
    R,
    L,
    F,
    B,
}

impl Face {
    pub const ALL: [Face; 6] = [Face::U, Face::D, Face::R, Face::L, Face::F, Face::B];

    /// Axis index: 0 = x (R/L), 1 = y (U/D), 2 = z (F/B).
    pub fn axis(self) -> usize {
        match self {
            Face::R | Face::L => 0,
            Face::U | Face::D => 1,
            Face::F | Face::B => 2,
        }
    }

    /// Direction of the outward normal along its axis.
    pub fn sign(self) -> i8 {
        match self {
            Face::R | Face::U | Face::F => 1,
            Face::L | Face::D | Face::B => -1,
        }
    }

    pub fn letter(self) -> char {
        match self {
            Face::U => 'U',
            Face::D => 'D',
            Face::R => 'R',
            Face::L => 'L',
            Face::F => 'F',
            Face::B => 'B',
        }
    }
}

/// A face turn. `turns`: 1 = clockwise (R), 2 = half turn (R2), 3 = counter-clockwise (R').
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Move {
    pub face: Face,
    pub turns: u8,
}

impl fmt::Display for Move {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let suffix = match self.turns {
            2 => "2",
            3 => "'",
            _ => "",
        };
        write!(f, "{}{}", self.face.letter(), suffix)
    }
}

pub fn to_string(moves: &[Move]) -> String {
    moves
        .iter()
        .map(|m| m.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Generate a random-move scramble:
///  * never turns the same face twice in a row
///  * never produces `R L R`-style sequences (same axis sandwiching)
pub fn generate<R: Rng>(rng: &mut R, len: usize) -> Vec<Move> {
    let mut moves: Vec<Move> = Vec::with_capacity(len);
    while moves.len() < len {
        let face = Face::ALL[rng.gen_range(0..6)];
        if let Some(last) = moves.last() {
            if last.face == face {
                continue;
            }
            if moves.len() >= 2
                && last.face.axis() == face.axis()
                && moves[moves.len() - 2].face == face
            {
                continue;
            }
        }
        moves.push(Move {
            face,
            turns: rng.gen_range(1..=3),
        });
    }
    moves
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrambles_obey_rules() {
        let mut rng = rand::thread_rng();
        for _ in 0..2000 {
            let s = generate(&mut rng, 20);
            assert_eq!(s.len(), 20);
            for w in s.windows(2) {
                assert_ne!(w[0].face, w[1].face);
            }
            for w in s.windows(3) {
                assert!(!(w[0].face == w[2].face && w[0].face.axis() == w[1].face.axis()));
            }
        }
    }

    #[test]
    fn formatting() {
        let m = [
            Move { face: Face::R, turns: 1 },
            Move { face: Face::U, turns: 3 },
            Move { face: Face::F, turns: 2 },
        ];
        assert_eq!(to_string(&m), "R U' F2");
    }
}

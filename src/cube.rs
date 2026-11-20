//! A tiny sticker-level cube model, used only to draw the scramble preview.
//!
//! Every sticker has an integer position (-1..=1 on each axis) and an outward
//! normal. A face turn rotates every sticker in that layer by 90 degrees, so
//! there are no permutation tables to get wrong.

use crate::scramble::{Face, Move};

#[derive(Clone, Copy)]
struct Sticker {
    pos: [i8; 3],
    normal: [i8; 3],
    color: Face,
}

#[derive(Clone)]
pub struct Cube {
    stickers: Vec<Sticker>,
}

/// +90 degree right-handed rotation about an axis.
fn rot90(v: [i8; 3], axis: usize) -> [i8; 3] {
    let [x, y, z] = v;
    match axis {
        0 => [x, -z, y],
        1 => [z, y, -x],
        _ => [-y, x, z],
    }
}

impl Cube {
    pub fn solved() -> Self {
        let mut stickers = Vec::with_capacity(54);
        for x in -1..=1i8 {
            for y in -1..=1i8 {
                for z in -1..=1i8 {
                    let pos = [x, y, z];
                    for face in Face::ALL {
                        let a = face.axis();
                        if pos[a] == face.sign() {
                            let mut normal = [0i8; 3];
                            normal[a] = face.sign();
                            stickers.push(Sticker { pos, normal, color: face });
                        }
                    }
                }
            }
        }
        Cube { stickers }
    }

    pub fn apply(&mut self, m: Move) {
        let a = m.face.axis();
        let s = m.face.sign();
        // A clockwise turn seen from outside the face is a right-handed
        // rotation of -sign * 90 degrees about the face's axis.
        let quarter_turns = (-(s as i32) * m.turns as i32).rem_euclid(4);
        for st in self.stickers.iter_mut().filter(|st| st.pos[a] == s) {
            for _ in 0..quarter_turns {
                st.pos = rot90(st.pos, a);
                st.normal = rot90(st.normal, a);
            }
        }
    }

    pub fn apply_all(&mut self, moves: &[Move]) {
        for &m in moves {
            self.apply(m);
        }
    }

    #[cfg(test)]
    pub fn is_solved(&self) -> bool {
        self.stickers.iter().all(|st| {
            let a = (0..3).find(|&i| st.normal[i] != 0).unwrap();
            Face::ALL
                .iter()
                .any(|f| f.axis() == a && f.sign() == st.normal[a] && *f == st.color)
        })
    }

    /// The 3x3 grid of sticker colours on `face`, as drawn in a standard net
    /// (U on top, L F R B in the middle, D at the bottom).
    pub fn face_grid(&self, face: Face) -> [[Face; 3]; 3] {
        let mut grid = [[face; 3]; 3];
        let a = face.axis();
        for st in self.stickers.iter().filter(|st| st.normal[a] == face.sign()) {
            let [x, y, z] = st.pos;
            let (row, col) = match face {
                Face::U => (z + 1, x + 1),
                Face::D => (1 - z, x + 1),
                Face::F => (1 - y, x + 1),
                Face::B => (1 - y, 1 - x),
                Face::R => (1 - y, 1 - z),
                Face::L => (1 - y, z + 1),
            };
            grid[row as usize][col as usize] = st.color;
        }
        grid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mv(face: Face, turns: u8) -> Move {
        Move { face, turns }
    }

    #[test]
    fn solved_net_is_uniform() {
        let c = Cube::solved();
        assert!(c.is_solved());
        for f in Face::ALL {
            assert!(c.face_grid(f).iter().flatten().all(|&x| x == f));
        }
    }

    #[test]
    fn four_quarter_turns_is_identity() {
        for f in Face::ALL {
            let mut c = Cube::solved();
            for _ in 0..4 {
                c.apply(mv(f, 1));
            }
            assert!(c.is_solved(), "{f:?}");
        }
    }

    #[test]
    fn move_then_inverse() {
        for f in Face::ALL {
            let mut c = Cube::solved();
            c.apply(mv(f, 1));
            assert!(!c.is_solved());
            c.apply(mv(f, 3));
            assert!(c.is_solved());
            c.apply(mv(f, 2));
            c.apply(mv(f, 2));
            assert!(c.is_solved());
        }
    }

    #[test]
    fn r_move_sends_front_right_column_to_up() {
        let mut c = Cube::solved();
        c.apply(mv(Face::R, 1));
        let u = c.face_grid(Face::U);
        for row in 0..3 {
            assert_eq!(u[row][2], Face::F);
            assert_eq!(u[row][0], Face::U);
        }
    }

    #[test]
    fn u_move_sends_front_to_left() {
        let mut c = Cube::solved();
        c.apply(mv(Face::U, 1));
        let l = c.face_grid(Face::L);
        assert!(l[0].iter().all(|&x| x == Face::F));
    }

    #[test]
    fn sexy_move_has_order_six() {
        let seq = [mv(Face::R, 1), mv(Face::U, 1), mv(Face::R, 3), mv(Face::U, 3)];
        let mut c = Cube::solved();
        for i in 0..6 {
            c.apply_all(&seq);
            assert_eq!(c.is_solved(), i == 5, "iteration {i}");
        }
    }
}

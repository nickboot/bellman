//! Helpers for testing circuit implementations.

use ff::{Field, PrimeField};
use pairing::Engine;

use crate::{ConstraintSystem, Index, LinearCombination, SynthesisError, Variable};

use std::collections::HashMap;
use std::fmt::Write;
use std::ops::{AddAssign, MulAssign};

use byteorder::{BigEndian, ByteOrder};
use std::cmp::Ordering;
use std::collections::BTreeMap;

use blake2s_simd::{Params as Blake2sParams, State as Blake2sState};

#[derive(Debug)]
enum NamedObject {
    Constraint(usize),
    Var(Variable),
    Namespace,
}

/// Constraint system for testing purposes.
#[allow(clippy::type_complexity)]
pub struct TestConstraintSystem<E: Engine> {
    named_objects: HashMap<String, NamedObject>,
    current_namespace: Vec<String>,
    constraints: Vec<(
        LinearCombination<E>,
        LinearCombination<E>,
        LinearCombination<E>,
        String,
    )>,
    inputs: Vec<(E::Fr, String)>,
    aux: Vec<(E::Fr, String)>,
}

#[derive(Clone, Copy)]
struct OrderedVariable(Variable);

impl Eq for OrderedVariable {}
impl PartialEq for OrderedVariable {
    fn eq(&self, other: &OrderedVariable) -> bool {
        match (self.0.get_unchecked(), other.0.get_unchecked()) {
            (Index::Input(ref a), Index::Input(ref b)) => a == b,
            (Index::Aux(ref a), Index::Aux(ref b)) => a == b,
            _ => false,
        }
    }
}
impl PartialOrd for OrderedVariable {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for OrderedVariable {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.0.get_unchecked(), other.0.get_unchecked()) {
            (Index::Input(ref a), Index::Input(ref b)) => a.cmp(b),
            (Index::Aux(ref a), Index::Aux(ref b)) => a.cmp(b),
            (Index::Input(_), Index::Aux(_)) => Ordering::Less,
            (Index::Aux(_), Index::Input(_)) => Ordering::Greater,
        }
    }
}

fn proc_lc<E: Engine>(terms: &LinearCombination<E>) -> BTreeMap<OrderedVariable, E::Fr> {
    let mut map = BTreeMap::new();
    for (var, &coeff) in terms.iter() {
        map.entry(OrderedVariable(var))
            .or_insert_with(E::Fr::zero)
            .add_assign(&coeff);
    }

    // Remove terms that have a zero coefficient to normalize
    let mut to_remove = vec![];
    for (var, coeff) in map.iter() {
        if coeff.is_zero().into() {
            to_remove.push(*var)
        }
    }

    for var in to_remove {
        map.remove(&var);
    }

    map
}

fn hash_lc<E: Engine>(terms: &LinearCombination<E>, h: &mut Blake2sState) {
    let map = proc_lc::<E>(terms);

    let mut buf = [0u8; 9 + 32];
    BigEndian::write_u64(&mut buf[0..8], map.len() as u64);
    h.update(&buf[0..8]);

    for (var, coeff) in map {
        match var.0.get_unchecked() {
            Index::Input(i) => {
                buf[0] = b'I';
                BigEndian::write_u64(&mut buf[1..9], i as u64);
            }
            Index::Aux(i) => {
                buf[0] = b'A';
                BigEndian::write_u64(&mut buf[1..9], i as u64);
            }
        }

        // Write as big-endian bytes.
        let mut bytes = coeff.to_repr();
        bytes.as_mut().reverse();
        buf[9..].copy_from_slice(&bytes.as_ref());

        h.update(&buf);
    }
}

fn eval_lc<E: Engine>(
    terms: &LinearCombination<E>,
    inputs: &[(E::Fr, String)],
    aux: &[(E::Fr, String)],
) -> E::Fr {
    let mut acc = E::Fr::zero();

    for (var, coeff) in terms.iter() {
        let mut tmp = match var.get_unchecked() {
            Index::Input(index) => inputs[index].0,
            Index::Aux(index) => aux[index].0,
        };

        tmp.mul_assign(coeff);
        acc.add_assign(&tmp);
    }

    acc
}

impl<E: Engine> TestConstraintSystem<E> {
    pub fn pretty_print(&self) -> String {
        let mut s = String::new();

        let negone = -E::Fr::one();

        let powers_of_two = (0..E::Fr::NUM_BITS)
            .map(|i| E::Fr::from(2u64).pow_vartime(&[u64::from(i)]))
            .collect::<Vec<_>>();

        let pp = |s: &mut String, lc: &LinearCombination<E>| {
            write!(s, "(").unwrap();
            let mut is_first = true;
            for (var, coeff) in proc_lc::<E>(&lc) {
                if coeff == negone {
                    write!(s, " - ").unwrap();
                } else if !is_first {
                    write!(s, " + ").unwrap();
                }
                is_first = false;

                if coeff != E::Fr::one() && coeff != negone {
                    for (i, x) in powers_of_two.iter().enumerate() {
                        if x == &coeff {
                            write!(s, "2^{} . ", i).unwrap();
                            break;
                        }
                    }

                    write!(s, "{:?} . ", coeff).unwrap();
                }

                match var.0.get_unchecked() {
                    Index::Input(i) => {
                        write!(s, "`{}`", &self.inputs[i].1).unwrap();
                    }
                    Index::Aux(i) => {
                        write!(s, "`{}`", &self.aux[i].1).unwrap();
                    }
                }
            }
            if is_first {
                // Nothing was visited, print 0.
                write!(s, "0").unwrap();
            }
            write!(s, ")").unwrap();
        };

        for &(ref a, ref b, ref c, ref name) in &self.constraints {
            writeln!(&mut s).unwrap();

            write!(&mut s, "{}: ", name).unwrap();
            pp(&mut s, a);
            write!(&mut s, " * ").unwrap();
            pp(&mut s, b);
            write!(&mut s, " = ").unwrap();
            pp(&mut s, c);
        }

        writeln!(&mut s).unwrap();

        s
    }

    pub fn hash(&self) -> String {
        let mut h = Blake2sParams::new().hash_length(32).to_state();
        {
            let mut buf = [0u8; 24];

            BigEndian::write_u64(&mut buf[0..8], self.inputs.len() as u64);
            BigEndian::write_u64(&mut buf[8..16], self.aux.len() as u64);
            BigEndian::write_u64(&mut buf[16..24], self.constraints.len() as u64);
            h.update(&buf);
        }

        for constraint in &self.constraints {
            hash_lc::<E>(&constraint.0, &mut h);
            hash_lc::<E>(&constraint.1, &mut h);
            hash_lc::<E>(&constraint.2, &mut h);
        }

        let mut s = String::new();
        for b in h.finalize().as_ref() {
            s += &format!("{:02x}", b);
        }

        s
    }

    pub fn which_is_unsatisfied(&self) -> Option<&str> {
        for &(ref a, ref b, ref c, ref path) in &self.constraints {
            let mut a = eval_lc::<E>(a, &self.inputs, &self.aux);
            let b = eval_lc::<E>(b, &self.inputs, &self.aux);
            let c = eval_lc::<E>(c, &self.inputs, &self.aux);

            a.mul_assign(&b);

            if a != c {
                return Some(&*path);
            }
        }

        None
    }

    pub fn is_satisfied(&self) -> bool {
        self.which_is_unsatisfied().is_none()
    }

    pub fn num_constraints(&self) -> usize {
        self.constraints.len()
    }

    pub fn set(&mut self, path: &str, to: E::Fr) {
        match self.named_objects.get(path) {
            Some(&NamedObject::Var(ref v)) => match v.get_unchecked() {
                Index::Input(index) => self.inputs[index].0 = to,
                Index::Aux(index) => self.aux[index].0 = to,
            },
            Some(e) => panic!(
                "tried to set path `{}` to value, but `{:?}` already exists there.",
                path, e
            ),
            _ => panic!("no variable exists at path: {}", path),
        }
    }

    pub fn verify(&self, expected: &[E::Fr]) -> bool {
        assert_eq!(expected.len() + 1, self.inputs.len());

        for (a, b) in self.inputs.iter().skip(1).zip(expected.iter()) {
            if &a.0 != b {
                return false;
            }
        }

        true
    }

    pub fn num_inputs(&self) -> usize {
        self.inputs.len()
    }

    pub fn get_input(&mut self, index: usize, path: &str) -> E::Fr {
        let (assignment, name) = self.inputs[index].clone();

        assert_eq!(path, name);

        assignment
    }

    pub fn get(&mut self, path: &str) -> E::Fr {
        match self.named_objects.get(path) {
            Some(&NamedObject::Var(ref v)) => match v.get_unchecked() {
                Index::Input(index) => self.inputs[index].0,
                Index::Aux(index) => self.aux[index].0,
            },
            Some(e) => panic!(
                "tried to get value of path `{}`, but `{:?}` exists there (not a variable)",
                path, e
            ),
            _ => panic!("no variable exists at path: {}", path),
        }
    }

    fn set_named_obj(&mut self, path: String, to: NamedObject) {
        if self.named_objects.contains_key(&path) {
            panic!("tried to create object at existing path: {}", path);
        }

        self.named_objects.insert(path, to);
    }
}

fn compute_path(ns: &[String], this: String) -> String {
    if this.chars().any(|a| a == '/') {
        panic!("'/' is not allowed in names");
    }

    let mut name = String::new();

    let mut needs_separation = false;
    for ns in ns.iter().chain(Some(&this).into_iter()) {
        if needs_separation {
            name += "/";
        }

        name += ns;
        needs_separation = true;
    }

    name
}

impl<E: Engine> ConstraintSystem<E> for TestConstraintSystem<E> {
    type Root = Self;

    fn new() -> TestConstraintSystem<E> {
        let mut map = HashMap::new();
        map.insert(
            "ONE".into(),
            NamedObject::Var(TestConstraintSystem::<E>::one()),
        );

        TestConstraintSystem {
            named_objects: map,
            current_namespace: vec![],
            constraints: vec![],
            inputs: vec![(E::Fr::one(), "ONE".into())],
            aux: vec![],
        }
    }

    fn alloc<F, A, AR>(&mut self, annotation: A, f: F) -> Result<Variable, SynthesisError>
    where
        F: FnOnce() -> Result<E::Fr, SynthesisError>,
        A: FnOnce() -> AR,
        AR: Into<String>,
    {
        let index = self.aux.len();
        let path = compute_path(&self.current_namespace, annotation().into());
        self.aux.push((f()?, path.clone()));
        let var = Variable::new_unchecked(Index::Aux(index));
        self.set_named_obj(path, NamedObject::Var(var));

        Ok(var)
    }

    fn alloc_input<F, A, AR>(&mut self, annotation: A, f: F) -> Result<Variable, SynthesisError>
    where
        F: FnOnce() -> Result<E::Fr, SynthesisError>,
        A: FnOnce() -> AR,
        AR: Into<String>,
    {
        let index = self.inputs.len();
        let path = compute_path(&self.current_namespace, annotation().into());
        self.inputs.push((f()?, path.clone()));
        let var = Variable::new_unchecked(Index::Input(index));
        self.set_named_obj(path, NamedObject::Var(var));

        Ok(var)
    }

    fn enforce<A, AR, LA, LB, LC>(&mut self, annotation: A, a: LA, b: LB, c: LC)
    where
        A: FnOnce() -> AR,
        AR: Into<String>,
        LA: FnOnce(LinearCombination<E>) -> LinearCombination<E>,
        LB: FnOnce(LinearCombination<E>) -> LinearCombination<E>,
        LC: FnOnce(LinearCombination<E>) -> LinearCombination<E>,
    {
        let path = compute_path(&self.current_namespace, annotation().into());
        let index = self.constraints.len();
        self.set_named_obj(path.clone(), NamedObject::Constraint(index));

        let a = a(LinearCombination::zero());
        let b = b(LinearCombination::zero());
        let c = c(LinearCombination::zero());

        self.constraints.push((a, b, c, path));
    }

    fn push_namespace<NR, N>(&mut self, name_fn: N)
    where
        NR: Into<String>,
        N: FnOnce() -> NR,
    {
        let name = name_fn().into();
        let path = compute_path(&self.current_namespace, name.clone());
        self.set_named_obj(path, NamedObject::Namespace);
        self.current_namespace.push(name);
    }

    fn pop_namespace(&mut self) {
        assert!(self.current_namespace.pop().is_some());
    }

    fn get_root(&mut self) -> &mut Self::Root {
        self
    }
}

#[test]
fn test_cs() {
    use blstrs::{Bls12, Scalar as Fr};

    let mut cs = TestConstraintSystem::<Bls12>::new();
    assert!(cs.is_satisfied());
    assert_eq!(cs.num_constraints(), 0);
    let a = cs
        .namespace(|| "a")
        .alloc(|| "var", || Ok(Fr::from(10u64)))
        .unwrap();
    let b = cs
        .namespace(|| "b")
        .alloc(|| "var", || Ok(Fr::from(4u64)))
        .unwrap();
    let c = cs.alloc(|| "product", || Ok(Fr::from(40u64))).unwrap();

    cs.enforce(|| "mult", |lc| lc + a, |lc| lc + b, |lc| lc + c);
    assert!(cs.is_satisfied());
    assert_eq!(cs.num_constraints(), 1);

    cs.set("a/var", Fr::from(4u64));

    let one = TestConstraintSystem::<Bls12>::one();
    cs.enforce(|| "eq", |lc| lc + a, |lc| lc + one, |lc| lc + b);

    assert!(!cs.is_satisfied());
    assert!(cs.which_is_unsatisfied() == Some("mult"));

    assert!(cs.get("product") == Fr::from(40u64));

    cs.set("product", Fr::from(16u64));
    assert!(cs.is_satisfied());

    {
        let mut cs = cs.namespace(|| "test1");
        let mut cs = cs.namespace(|| "test2");
        cs.alloc(|| "hehe", || Ok(Fr::one())).unwrap();
    }

    assert!(cs.get("test1/test2/hehe") == Fr::one());
}

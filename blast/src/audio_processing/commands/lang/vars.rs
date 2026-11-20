//! `let` variables (REQ-056 §6): a session table, and the pass that replaces
//! `$name` and `(expr)` by plain numbers before a statement is lowered, so
//! objects store numbers and `state` shows those.

use super::ast::*;
use super::errors::{Code, LangErr, Span};
use super::schema::db_to_linear;
use std::collections::HashMap;

/// What a `let` holds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VarValue {
    Num(f64),
    Duration(f64, DurUnit),
}

/// The session's variables.
#[derive(Clone, Debug, Default)]
pub struct Vars {
    map: HashMap<String, VarValue>,
}

impl Vars {
    /// Stores (or replaces) `name`.
    pub fn set(&mut self, name: &str, value: VarValue) {
        self.map.insert(name.to_string(), value);
    }

    /// The value of `name`.
    pub fn get(&self, name: &str) -> Option<VarValue> {
        self.map.get(name).copied()
    }

    fn lookup(&self, id: &Ident) -> Result<VarValue, LangErr> {
        self.get(&id.name).ok_or_else(|| {
            LangErr::new(
                Code::UnknownName,
                id.span,
                format!("unknown variable '${}'", id.name),
            )
            .with_suggestion(&id.name, self.map.keys().map(String::as_str))
        })
    }
}

fn not_a_number(id: &Ident, what: &str) -> LangErr {
    LangErr::new(
        Code::WrongType,
        id.span,
        format!("'${}' holds {what}, and a number is needed here", id.name),
    )
}

fn var_number(vars: &Vars, id: &Ident) -> Result<f64, LangErr> {
    match vars.lookup(id)? {
        VarValue::Num(v) => Ok(v),
        VarValue::Duration(..) => Err(not_a_number(id, "a duration")),
    }
}

/// The value of an expression; division by zero is E403.
pub fn eval_expr(vars: &Vars, e: &Expr, span: Span) -> Result<f64, LangErr> {
    match e {
        Expr::Num(v) => Ok(*v),
        Expr::Var(id) => var_number(vars, id),
        Expr::Neg(inner) => Ok(-eval_expr(vars, inner, span)?),
        Expr::Bin(a, op, b) => {
            let (a, b) = (eval_expr(vars, a, span)?, eval_expr(vars, b, span)?);
            Ok(match op {
                BinOp::Add => a + b,
                BinOp::Sub => a - b,
                BinOp::Mul => a * b,
                BinOp::Div if b == 0.0 => {
                    return Err(LangErr::new(Code::OutOfRange, span, "division by zero"));
                }
                BinOp::Div => a / b,
            })
        }
    }
}

/// The value of a number.
pub fn eval(vars: &Vars, n: &Num) -> Result<f64, LangErr> {
    let v = match n {
        Num::Lit(v, _) => *v,
        Num::Var(id) => var_number(vars, id)?,
        Num::Expr(e, span) => eval_expr(vars, e, *span)?,
    };
    if v.is_finite() {
        Ok(v)
    } else {
        Err(LangErr::new(
            Code::OutOfRange,
            n.span(),
            "the number is too large",
        ))
    }
}

fn subst_num(vars: &Vars, n: &mut Num) -> Result<(), LangErr> {
    if !matches!(n, Num::Lit(..)) {
        let span = n.span();
        *n = Num::Lit(eval(vars, n)?, span);
    }
    Ok(())
}

fn subst_clock(vars: &Vars, c: &mut ClockLit) -> Result<(), LangErr> {
    // a bare `$var` takes its unit from the variable
    if c.unit.is_none()
        && let Num::Var(id) = &c.n
    {
        return match vars.lookup(id)? {
            VarValue::Duration(n, unit) => {
                c.unit = Some(unit);
                c.n = Num::Lit(n, id.span);
                match &mut c.over {
                    Some(o) => subst_value(vars, &mut o.value),
                    None => Ok(()),
                }
            }
            _ => Err(LangErr::new(
                Code::WrongType,
                id.span,
                "a clock needs a unit: ($x)b",
            )),
        };
    }
    subst_num(vars, &mut c.n)?;
    if let Some(o) = &mut c.over {
        subst_value(vars, &mut o.value)?;
    }
    Ok(())
}

fn subst_value(vars: &Vars, v: &mut Value) -> Result<(), LangErr> {
    match v {
        // a bare `$x` can hold a duration, and then it is one
        Value::Num(Num::Var(id)) => match vars.lookup(id)? {
            VarValue::Num(n) => *v = Value::Num(Num::Lit(n, id.span)),
            VarValue::Duration(n, unit) => {
                *v = Value::Duration(Num::Lit(n, id.span), unit);
            }
        },
        Value::Num(n)
        | Value::Typed(n, _)
        | Value::Duration(n, _)
        | Value::Db(n)
        | Value::Pitch(n, _) => subst_num(vars, n)?,
        Value::Word(_) | Value::Words(_) | Value::Str(_) => {}
    }
    Ok(())
}

fn subst_step(vars: &Vars, s: &mut Step) -> Result<(), LangErr> {
    subst_num(vars, &mut s.at)?;
    if let Some(n) = &mut s.to {
        subst_num(vars, n)?;
    }
    if let Some(n) = &mut s.chance {
        subst_num(vars, n)?;
    }
    if let Some(n) = &mut s.jitter {
        subst_num(vars, n)?;
    }
    Ok(())
}

fn subst_items(vars: &Vars, items: &mut [Item]) -> Result<(), LangErr> {
    for item in items {
        match item {
            Item::From(..) | Item::FromName(_) | Item::On(ClockRef::Path(_)) => {}
            Item::On(ClockRef::Lit(c)) | Item::Clock(c) => subst_clock(vars, c)?,
            Item::In(n) => subst_num(vars, n)?,
            Item::Setting(_, v, over) => {
                subst_value(vars, v)?;
                if let Some(o) = over {
                    subst_value(vars, &mut o.value)?;
                }
            }
            Item::Step(s) => subst_step(vars, s)?,
        }
    }
    Ok(())
}

/// Replaces every variable and expression in `stmt` (and its block) by the
/// plain value; the first problem is the error (E301 unknown variable, E402
/// a duration where a number is needed, E403 division by zero).
pub fn substitute(vars: &Vars, stmt: &mut Stmt) -> Result<(), LangErr> {
    match stmt {
        Stmt::Object(o) => {
            subst_items(vars, &mut o.items)?;
            if let Some(block) = &mut o.block {
                for line in &mut block.lines {
                    for s in line.stmts_mut() {
                        substitute(vars, s)?;
                    }
                }
            }
            if let Some(receivers) = &mut o.pipe {
                for r in receivers {
                    subst_items(vars, &mut r.items)?;
                }
            }
            Ok(())
        }
        Stmt::Let(l) => subst_value(vars, &mut l.value),
        Stmt::Action(_) | Stmt::Source(_) | Stmt::Query(_) | Stmt::End(_) => Ok(()),
    }
}

/// What a `let` stores: a number, a duration (E402
/// otherwise). The value has been through `substitute`.
pub fn let_value(vars: &Vars, l: &Let) -> Result<VarValue, LangErr> {
    match &l.value {
        Value::Num(n) => Ok(VarValue::Num(eval(vars, n)?)),
        Value::Duration(n, unit) => Ok(VarValue::Duration(eval(vars, n)?, *unit)),
        // decibels are stored as the linear gain they mean
        Value::Db(n) => Ok(VarValue::Num(db_to_linear(eval(vars, n)?))),
        Value::Typed(n, _) => Ok(VarValue::Num(eval(vars, n)?)),
        Value::Words(w) => Err(LangErr::new(
            Code::WrongType,
            w[0].span,
            "a variable holds a number or a duration",
        )),
        Value::Word(id) => Err(LangErr::new(
            Code::WrongType,
            id.span,
            "a variable holds a number or a duration",
        )),
        Value::Pitch(..) => Err(LangErr::new(
            Code::WrongType,
            l.span,
            "a variable holds a number or a duration, not a pitch (write the ratio, or 7st)",
        )),
        Value::Str(_) => Err(LangErr::new(
            Code::WrongType,
            l.span,
            "a variable holds a number or a duration, not a string",
        )),
    }
}

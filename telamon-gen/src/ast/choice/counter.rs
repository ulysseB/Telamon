use std::iter;

use crate::ast::constrain::Constraint;
use crate::ast::context::CheckerContext;
use crate::ast::error::TypeError;
use crate::ast::{
    type_check_code, type_check_enum_values, ChoiceDef, ChoiceInstance, Condition,
    CounterBody, CounterVal, VarDef, VarMap,
};
use crate::ir::{self, Adaptable};
use crate::lexer::Spanned;
use fxhash::FxHashSet;
use itertools::Itertools;
use log::trace;
use utils::RcStr;

#[derive(Clone, Debug)]
pub struct CounterDef {
    pub name: Spanned<RcStr>,
    pub doc: Option<String>,
    pub visibility: ir::CounterVisibility,
    pub vars: Vec<VarDef>,
    pub body: CounterBody,
}

impl CounterDef {
    /// Creates an action to update the a counter when incr is modified.
    #[allow(clippy::too_many_arguments)]
    fn gen_incr_counter(
        &self,
        counter: &RcStr,
        num_counter_args: usize,
        var_map: &VarMap,
        incr: &ir::ChoiceInstance,
        incr_condition: &ir::ValueSet,
        value: ir::CounterVal,
        ir_desc: &mut ir::IrDesc,
    ) -> ir::OnChangeAction {
        // Adapt the environement to the point of view of the increment.
        let (forall_vars, set_constraints, adaptator) =
            ir_desc.adapt_env(var_map.env(), incr);
        let value = value.adapt(&adaptator);
        let counter_vars = (0..num_counter_args)
            .map(|i| adaptator.variable(ir::Variable::Arg(i)))
            .collect();
        let action = ir::ChoiceAction::IncrCounter {
            counter: ir::ChoiceInstance {
                choice: counter.clone(),
                vars: counter_vars,
            },
            value: value.adapt(&adaptator),
            incr_condition: incr_condition.adapt(&adaptator),
        };
        ir::OnChangeAction {
            forall_vars,
            set_constraints,
            action,
        }
    }

    /// Returns the `CounterVal` referencing a choice. Registers the UpdateCounter action
    /// so that the referencing counter is updated when the referenced counter is changed.
    #[allow(clippy::too_many_arguments)]
    fn counter_val_choice(
        &self,
        counter: &ChoiceInstance,
        caller_visibility: ir::CounterVisibility,
        caller: RcStr,
        incr: &ir::ChoiceInstance,
        incr_condition: &ir::ValueSet,
        kind: ir::CounterKind,
        num_caller_vars: usize,
        var_map: &VarMap,
        ir_desc: &mut ir::IrDesc,
    ) -> (ir::CounterVal, ir::OnChangeAction) {
        // TODO(cleanup): do not force an ordering on counter declaration.
        let value_choice = ir_desc.get_choice(&counter.name);
        match *value_choice.choice_def() {
            ir::ChoiceDef::Counter {
                visibility,
                kind: value_kind,
                ..
            } => {
                // TODO(cleanup): allow mul of sums. The problem is that you can multiply
                // and/or divide by zero when doing this.
                use crate::ir::CounterKind;
                assert!(!(kind == CounterKind::Mul && value_kind == CounterKind::Add));
                assert!(
                    caller_visibility >= visibility,
                    "Counters cannot sum on counters that expose less information"
                );
            }
            ir::ChoiceDef::Number { .. } => (),
            ir::ChoiceDef::Enum { .. } => panic!("Enum as a counter value"),
        };
        // Type the increment counter value in the calling counter context.
        let instance = counter.type_check(&ir_desc, var_map);
        let (forall_vars, set_constraints, adaptator) =
            ir_desc.adapt_env(var_map.env(), &instance);
        let caller_vars = (0..num_caller_vars)
            .map(ir::Variable::Arg)
            .map(|v| adaptator.variable(v))
            .collect();
        // Create and register the action.
        let action = ir::ChoiceAction::UpdateCounter {
            counter: ir::ChoiceInstance {
                choice: caller,
                vars: caller_vars,
            },
            incr: incr.adapt(&adaptator),
            incr_condition: incr_condition.adapt(&adaptator),
        };
        let update_action = ir::OnChangeAction {
            forall_vars,
            set_constraints,
            action,
        };
        (ir::CounterVal::Choice(instance), update_action)
    }

    /// Creates a choice to store the increment condition of a counter. Returns the
    /// corresponding choice instance from the point of view of the counter and the
    /// condition on wich the counter must be incremented.
    #[allow(clippy::too_many_arguments)]
    fn gen_increment(
        &self,
        counter: &str,
        counter_vars: &[(RcStr, ir::Set)],
        iter_vars: &[(RcStr, ir::Set)],
        all_vars_defs: Vec<VarDef>,
        conditions: Vec<Condition>,
        var_map: &VarMap,
        ir_desc: &mut ir::IrDesc,
        constraints: &mut Vec<Constraint>,
    ) -> (ir::ChoiceInstance, ir::ValueSet) {
        // TODO(cleanup): the choice the counter increment is based on must be declared
        // before the increment. It should not be the case.
        if let [Condition::Is {
            ref lhs,
            ref rhs,
            is,
        }] = conditions[..]
        {
            let incr = lhs.type_check(&ir_desc, var_map);
            // Ensure all forall values are usefull.
            let mut foralls = FxHashSet::default();
            for &v in &incr.vars {
                if let ir::Variable::Forall(i) = v {
                    foralls.insert(i);
                }
            }
            if foralls.len() == iter_vars.len() {
                // Generate the increment condition.
                let choice = ir_desc.get_choice(&incr.choice);
                let enum_ = ir_desc.get_enum(choice.choice_def().as_enum().unwrap());
                let values = type_check_enum_values(enum_, rhs.clone());
                let values = if is {
                    values
                } else {
                    enum_
                        .values()
                        .keys()
                        .filter(|&v| !values.contains(v))
                        .cloned()
                        .collect()
                };
                return (
                    incr,
                    ir::ValueSet::enum_values(enum_.name().clone(), values),
                );
            }
        }
        // Create the new choice.
        let bool_choice: RcStr = "Bool".into();
        let name = RcStr::new("increment_".to_string() + counter);
        let def = ir::ChoiceDef::Enum(bool_choice.clone());
        let variables = counter_vars.iter().chain(iter_vars).cloned().collect();
        let args = ir::ChoiceArguments::new(variables, false, false);
        let incr_choice = ir::Choice::new(name.clone(), None, args, def);
        ir_desc.add_choice(incr_choice);
        // Constraint the boolean to follow the conditions.
        let vars = counter_vars
            .iter()
            .chain(iter_vars)
            .map(|x| x.0.clone())
            .collect();
        let incr_instance = ChoiceInstance {
            name: name.clone(),
            vars,
        };
        let is_false = Condition::new_is_bool(incr_instance, false);
        let mut disjunctions = conditions
            .iter()
            .map(|cond| vec![cond.clone(), is_false.clone()])
            .collect_vec();
        disjunctions.push(
            iter::once(is_false)
                .chain(conditions)
                .map(|mut cond| {
                    cond.negate();
                    cond
                })
                .collect(),
        );
        constraints.push(Constraint::new(all_vars_defs, disjunctions));
        // Generate the choice instance.
        let vars = (0..counter_vars.len())
            .map(ir::Variable::Arg)
            .chain((0..iter_vars.len()).map(ir::Variable::Forall))
            .collect();
        let true_value = iter::once("TRUE".into()).collect();
        let condition = ir::ValueSet::enum_values(bool_choice, true_value);
        (ir::ChoiceInstance { choice: name, vars }, condition)
    }

    /// Registers a counter in the ir description.
    pub fn register_counter(
        &self,
        ir_desc: &mut ir::IrDesc,
        constraints: &mut Vec<Constraint>,
    ) {
        trace!("defining counter {}", self.name.data.to_owned());
        println!("defining counter {}", self.name.data.to_owned());

        let mut var_map = VarMap::default();
        // Type-check the base.
        let kind = self.body.kind;
        let all_var_defs = self
            .vars
            .to_owned()
            .iter()
            .chain(&self.body.iter_vars)
            .cloned()
            .collect();
        let vars = self
            .vars
            .iter()
            .map(|def| {
                (
                    def.name.clone(),
                    var_map.decl_argument(&ir_desc, def.to_owned()),
                )
            })
            .collect_vec();
        let base = type_check_code(RcStr::new(self.body.to_owned().base), &var_map);
        // Generate the increment
        let iter_vars = self
            .body
            .to_owned()
            .iter_vars
            .into_iter()
            .map(|def| (def.name.clone(), var_map.decl_forall(&ir_desc, def)))
            .collect_vec();
        let doc = self.doc.to_owned().map(RcStr::new);
        let (incr, incr_condition) = self.gen_increment(
            &self.name.data.to_owned(),
            vars.iter()
                .cloned()
                .map(|(n, s)| (n.data, s))
                .collect::<Vec<_>>()
                .as_slice(),
            iter_vars
                .iter()
                .cloned()
                .map(|(n, s)| (n.data, s))
                .collect::<Vec<_>>()
                .as_slice(),
            all_var_defs,
            self.body.to_owned().conditions,
            &var_map,
            ir_desc,
            constraints,
        );
        // Type check the value.
        let value = match self.body.value {
            CounterVal::Code(ref code) => ir::CounterVal::Code(type_check_code(
                RcStr::new(code.to_owned()),
                &var_map,
            )),
            CounterVal::Choice(ref counter) => {
                let counter_name = self.name.data.to_owned();
                let (value, action) = self.counter_val_choice(
                    &counter,
                    self.visibility.to_owned(),
                    counter_name,
                    &incr,
                    &incr_condition,
                    kind,
                    vars.len(),
                    &var_map,
                    ir_desc,
                );
                ir_desc.add_onchange(&counter.name, action);
                value
            }
        };
        let incr_counter = self.gen_incr_counter(
            &self.name.data.to_owned(),
            vars.len(),
            &var_map,
            &incr,
            &incr_condition,
            value.clone(),
            ir_desc,
        );
        ir_desc.add_onchange(&incr.choice, incr_counter);
        // Register the counter choices.
        let incr_iter = iter_vars.iter().map(|p| p.1.clone()).collect_vec();
        let counter_def = ir::ChoiceDef::Counter {
            incr_iter,
            kind,
            value,
            incr,
            incr_condition,
            visibility: self.visibility.to_owned(),
            base,
        };
        let counter_args = ir::ChoiceArguments::new(
            vars.into_iter().map(|(n, s)| (n.data, s)).collect(),
            false,
            false,
        );
        let mut counter_choice =
            ir::Choice::new(self.name.data.to_owned(), doc, counter_args, counter_def);
        // Filter the counter itself after an update, because the filter actually acts on
        // the increments and depends on the counter value.
        let filter_self = ir::OnChangeAction {
            forall_vars: vec![],
            set_constraints: ir::SetConstraints::default(),
            action: ir::ChoiceAction::FilterSelf,
        };
        counter_choice.add_onchange(filter_self);
        ir_desc.add_choice(counter_choice);
    }

    pub fn define(
        self,
        context: &mut CheckerContext,
        choice_defs: &mut Vec<ChoiceDef>,
    ) -> Result<(), TypeError> {
        choice_defs.push(ChoiceDef::CounterDef(self));
        Ok(())
    }
}

impl PartialEq for CounterDef {
    fn eq(&self, rhs: &Self) -> bool {
        self.name == rhs.name
    }
}

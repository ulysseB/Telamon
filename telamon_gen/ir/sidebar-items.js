initSidebarItems({"enum":[["ChoiceAction","An action to perform,"],["ChoiceArguments","Defines the parameters for which the `Choice` is defined."],["ChoiceDef","Specifies how the `Choice` is defined."],["CmpOp","A compariason operator."],["Condition","A condition producing a boolean."],["CounterKind","Indicates whether a counter sums or adds."],["CounterVal","The value of the increments of a counter."],["CounterVisibility","Indicates how a counter exposes how its maximum value. The variants are ordered by increasing amount of information available."],["FilterRef","References a filter to call."],["SetDefKey",""],["SubFilter","Filters the set of valid values, given some inputs."],["ValueSet","Represents a set of values a choice can take."],["ValueType","Specifies the type of the values a choice can take."],["Variable",""]],"fn":[["dummy_choice",""],["normalized_enum_set","Creates a `ValueSet` from the list of enum values."]],"struct":[["Adaptator","Represent a transformation to apply to a rule to fir it in a new context."],["Choice","A decision to specify."],["ChoiceCondition","A condition from the point of view of a choice."],["ChoiceInstance","An choice instantiated with the given variables."],["Code","A piece of rust code."],["Enum","A choice that can take a few predefined values."],["Filter","Filters the set valid values."],["FilterAction","Restricts the set of valid values."],["FilterCall","A call to a filter."],["IrDesc","Describes the choices that constitute the IR."],["OnChangeAction","An action to perform when the choice is restricted."],["OnNewObject","Indicates how to update the search space when a new object is added to the set. Assumes the set is mapped to `ir::Variable::Arg(0)` and its argument to `ir::Variable::Arg(1)` if any."],["RemoteFilterCall","A call to a filter in another choice."],["Rule","Specifies a conditional restriction on the set of valid values."],["Set","References a set of objects."],["SetConstraints","A list of constraints on the set each variable belongs to. It must be built using `SetConstraints::new` so the constraints are in the right order."],["SetDef","Defines a set of objects."],["SetRefImpl",""],["Trigger","A piece of host code called when a list of conditions are met."]],"trait":[["Adaptable",""],["SetRef","Generic trait for sets."]]});
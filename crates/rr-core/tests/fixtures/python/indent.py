def before(value):
    return value


class Service:
    def run(self, value):
        result = helper(value)
      return result


def after(value):
    return helper(value)
